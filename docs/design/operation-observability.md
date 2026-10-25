# operation受付と後続処理の観測

ユーザー採用のローカル拡張。採用済みのCLI契約にある「送信受付と処理の成功は別」「旧instanceは保存結果の照会のみ」を実装へ揃える。既存のoperation ID、receipt、対象状態を再利用し、共通jobエンジンや外部副作用のexactly-once保証は追加しない。観測field自体は既存schemaを使い、後述のreceipt予算にはschema v6、開発途中版との整合にはv8を使う。

## 保存済み結果と再送

公開17 mutationを下表で監査した。DB mutationは変更と成功/失敗receiptを同一transactionへ保存する。runtime操作はpending intentを先にcommitし、副作用後にreceiptを確定する。要求のcommand/argsと認証済みcallerをfingerprint照合し、同じIDの別内容はoperation_conflict。保存結果のある旧instance IDは結果照会のみ、未記録の旧IDはoperation_expired。pending intentは再実行許可ではなく、同一IDでも副作用を二度開始しない。

構文・認証・保存前preflightの拒否にはreceiptが存在しない場合がある。これは受付済み失敗とは区別する。receipt照会操作自体も保存結果を返すため、状態を更新して観測するときは新しい照会operation IDを使う。旧instanceの失効したorigin tokenに新たな権限は与えない。

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

## Receiptの有限予算（P2/W5/C10.2）

現在instanceの同ID再送結果は終了まで不変で、read-onlyかどうかとは独立する。読取receiptを一律除外せず、`Status` だけは従来どおりreceipt不要の現在値projectionとする。cursorの有効性・検索snapshot寿命をreceipt保持と同一視しない。

内部既定は保存outcome1件8MiB、operation_receiptのaggregate256MiB。compact.result等の後続metadataはこのreceipt専用上限の外で、既存の全体logical data上限へ引き続き算入し、旧receipt回収時に併せて削除する。各rowのID・origin・hash・時刻・結果と512bytesの管理費、実行中の未使用result予約を算入する。これはSQLiteの物理使用量上限ではない。未知サイズのruntime結果は副作用前に8MiBを予約し、固定ackも現行実装では同じ8MiBを予約する（後述の同時受付上限）。runtime_preflightの512KiBは輸送frameの検証用で、予約byte数を下げるものではない。通常DB操作は同transactionで事前予約を確認し、結果超過はsavepoint内の変更をrollbackする。runtime結果は開始前に上限を保証し、GCの詳細配列には件数上限と総数・省略表示を持たせる。保存outcomeに加え、operation IDやerror hintを含む完全なResponse envelopeが輸送8MiBに収まることも保存前に確認する。予約と確定は同じDB worker順序で行い、確定後に未使用予約を解放する。

予算到達は副作用前の`storage_limit`で拒否し、拒否そのものを新receiptとして蓄積しない。同IDの既存receipt照会は容量検査に先行する。任意回数の新operation receiptを有限な非常用poolへ保存する解決は採らない。receipt不要の`shellboy status --operation ID`を追加し、既存receiptの状態・不確実性・対象ID・予算を読み取れるようにする。全文結果は認証が有効な元callerから元の要求/同IDを再送して得る。失効したOriginでは認証を迂回せず、statusで状態/対象を確認し、容量回復後にoperation getを使う。従来の`operation get`の保存済み結果を変更しない。現在receiptだけで容量が満ちた場合も内部GC（開始時/60秒ごと）とstatus・同ID照会・通常終了は妨げず、明示的なowner再起動で旧terminal receiptを回収できる。動作中ownerを自動再起動しない。

旧instanceのterminal receiptは、accepted/held message・未反映添付・queued/inflight compactによる参照がなければ短いbatchで回収できる。通常は7日を目安とし、容量圧迫時には早期回収する。関連するcompact.resultも一体で回収する。現instanceの全rowと、旧instanceの未確定intentは保護する。旧pendingの8MiB実行中予約は新instanceでは不要なので実metadata量だけ算入し、結果を捏造せず不確実性を維持する。保護対象だけで上限に達した場合も無断削除をせずstatusに示す。旧receipt回収後は元IDで副作用を再実行せずoperation_expired、operation get/status照会はnot foundとなる。

新規worker・表・event・依存は追加しない。予算計算/予約/旧terminal回収は小さなControl内のreceipt専用moduleへ分離する。永続予約が必要な場合は既存operation_receiptへの列追加に限定し、runtime workerが所有し、確定またはinstance終了で予約部分を解放する。検証では小さい注入予算を使い、巨大fixtureを生成せず、並行予約・拒否前副作用なし・同ID再送・旧terminal回収・旧pending保護・full時status/内部GCを確認する。

`status --operation`の対象IDは1024bytes以下とし、作業一覧filterとの併用は拒否する。statusの通常結果にもreceipt_budgetを追加する。lookupはoutcome全文を複写せず、固定metadataと各1024bytes以内のtarget IDだけを返し、長いtarget IDはtargets_omittedで明示する。確定済み結果のcursor/snapshotを再構築する機能ではない。schema v6は予約byte数の列1件・pending用部分index・既存quota_counter内の導出row2件・保守trigger3件を追加し、runtime intentの確定で予約を0に戻す。既存rowの値は0、旧pendingの結果不明状態を維持する。

予算到達時の先行回収は1回128rowまでとし、そのbatchだけで受付可能にならなくても、成功した旧terminal回収はStorageLimit拒否前にcommitする。新要求のreceiptや副作用は作らない。回収中の保存失敗はcompanion metadataとともにrollbackする。実使用量は既存quota_counterの導出row2件と3triggerで保ち、pending予約だけを部分indexで読む。旧pendingの保護量も導出pending量からcurrent pending量を引き、保存result全文の走査を避ける。計測と範囲は receipt budget検証（非同梱の検証記録）。


## 現instance満杯と同時予約の制限（PR1-3）

256MiBは「完了結果の実metadata/outcome量＋current pendingの未使用予約」の上限である。
読取・wait・固定ackも新operation IDなら最初に8MiB＋row管理512bytes＋ID/origin/hash/時刻の
実byte数を予約する。短いmetadataの空fixtureでも同時pendingは31件までで、32件目は副作用前に
拒否される。既存の完了結果や長いmetadataがあればさらに少ない。これはIPC接続/work枠と別の
上限であり、31本のshellやThreadを許可する意味ではない。pending完了で未使用予約が0になると
通常受付枠が戻る。完了ackが小さくても処理中の予約は小さくならない、保守的な現行制限を採る。

完了/失敗も現instance中はGCしない。小さい読取結果の高頻度pollingでも実bytesが累積し、
次の8MiB予約とmetadataを置けなくなると、使用量256MiB未満（典型的には約248MiB）でも新規
受付を拒否する。`full_result_admission_blocked`は8MiB本体の余地を示し、任意長metadataを含む
次の要求の受付保証ではない。`at_limit=false`も受付可能を保証しない。満杯までの時間は
結果サイズ・文字escape・ページ幅・操作頻度・並行予約に依存し、固定の稼働時間保証はない。
実操作の標本bytesと頻度例はreceipt予算検証（非同梱の検証記録）へ記録する。

満杯時は新IDの`storage status`/`storage collect`も同じ予約で拒否される。`run.stop`/`shell.stop`も
新receiptを必要とし、IPCの停止用優先枠がreceipt容量を保証するわけではない。owner自体の通常
終了は新CLI receiptを必要としない。回復経路は
receipt-freeの`shellboy status`/`shellboy status --operation ID`、元のcommand/args/callerを保った同ID再送、
owner内部の定期GCと通常終了、作業終了後の明示restartである。Status/再送は新rowを作らず、
内部GCはCLI receipt予約を経ない。ただし内部GCがcurrent結果を捨てることはない。restart後も
旧pending/accepted・held等からの参照保護は維持し、未確定副作用を再実行しない。予約部分だけが
新instanceでは不要になる。回収可能な旧terminalは通常7日/容量圧迫時の短いbatchで回収し、
不足が残れば次の内部GC/受付時回収を待つ。保護された実metadataだけで満杯ならrestartだけで
必ず回復するとは約束しない。無断失効、新しい非常用receipt pool、TTL/表/workerは加えない。
