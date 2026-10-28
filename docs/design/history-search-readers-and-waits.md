# 履歴semantic検索の読取分離とwait通知

Issue #27 / #28の採用設計。検証結果はacceptanceへ別途記録する。

## semantic検索

IndexServiceの検索は、query embedding前の設定・選択space照合と旧query-profile markerの補完だけを既存writerへ渡す。候補走査、既存Witnessの検証、vec0 KNN、件数不足時の正確なL2、provenance読取りは、別のread-only SQLite接続で行う。検索自体の有料embeddingは従来どおり1回で、検索から索引を作らない。

IndexServiceが専用semaphoreを1個所有する。同時に実行できる検索は1個で、待機queueは作らない。slotが空いていなければquery embedding前にInvalidStateで再試行を案内する。slotはembedding HTTPの開始前から、ランキング・再照合・接続閉鎖まで同じ1個を保持する。子Threadの同時検索も同じownerのslotを共有し、先行HTTPが遅い間も即座に拒否する。これは同時受付を無限に待たせないための局所admissionであり、汎用のpool、worker、job engineではない。

接続の作成から閉鎖までは既存Tokio runtimeのspawn_blocking closureが担当する。closureがslot permitを持ち、async観測が取り消されても実処理が終了するまで解放しない。接続はSQLITE_OPEN_READ_ONLYとquery_onlyを使い、migration・書込みfallbackをしない。元のDbWorkerが保持する実DB pathを使い、既定のファイル名を推測しない。

callerの切断、futureのdrop、owner停止は検索専用tokenを取消し、登録済みSQLite InterruptHandleへinterruptを送る。行走査・KNNの再試行・L2走査・結果再検証の境界でも取消しを確認する。取消し由来のSQLiteエラーをSaveFailedへ写してownerを停止させず、観測のInvalidStateとして返す。read-only接続のopen、busy/lock、query、行読取り、読取transactionの終了、再照合の保存エラー分類は、このclosureの出口だけでIoFailedへ写す。元のSQLite原因はmessageに残す。reader境界でwriter向けのcapacity/permissions hintを置き換え、読取原因の調査・修復と、HTTP後の再試行にはembedding再課金の可能性があることを案内する。共用helperと、検索前のwriter上のquery-profile marker更新のSaveFailedは変更せず、真のwriter失敗は従来どおりownerを停止させる。SQLite拡張内部の単一計算を常に一定ms以内に中断するSLAは設けない。

ランキング開始時のread snapshotで選択spaceのidentityと全適格Witnessを確認する。ランキング後はsnapshotを解放し、新しいsnapshotで選択spaceと返却候補最大50nodeのWitness、raw retention、tombstone、現在の検索可視性、明示Threadの採用経路/置換source所属を再確認する。変更された候補は除外する。winning spanのWitnessが変わった候補のdistanceを使い回さず、その候補全体を除外するため、要求件数より少なくなる場合がある。これはDB全体の長期snapshotや原文保持の保証ではない。

終了時は既存IndexService.wait_idleがsource処理に加え検索slotもdrainし、接続が閉じる前にowner DBを閉じない。UIのメインOS threadでI/Oやjoinを待たない。

## wait通知

Controlの既存changed NotifyはRun/Turn/shellの状態・受付等の再観測に残す。raw chunkとsaved_sequenceだけを保存したcommitは、別のshell_output_changed Notifyだけへ通知する。

ShellOutput条件があるwaitだけが出力Notifyを購読する。すべてのwaitは従来のchangedも購読するので、ShellEnded、TurnCompleted、RunEnded、recovery_wait、取消し、timeoutの意味は変わらない。両Notifyを必要に応じてenableしてからDBを確認し、確認とawaitの間の通知を取りこぼさない。保存失敗では出力成功通知を発行しない。

出力Notifyはownerごとに1個で、execution ID別のmapやworkerは作らない。複数のShellOutput待ち相互のfanoutは残るが、通常の終了/Turn待ちを出力batchごとに起こす負荷を除く。将来対象ID別の分配が必要かは、この最小変更後の実測で判断する。

## 保守集計

今回の読取分離は明示semantic検索へ限定する。GCのraw_stats等は回収・容量判定と同じ保守処理の一部であり、集計だけを別snapshotへ切り離すと、その集計と後続の書込み判断の時点が分かれる。計算量と同じtransactionでの再照合を含む別の設計が必要になる。今回は新しい共通読取poolを作らず、既存writer上の集計を維持し、関連索引の実測改善はIssue #26で扱う。

## Reopened #27: admission and recovery decision

Keep one admission permit across query embedding HTTP and SQLite ranking. Moving admission after HTTP would allow concurrent children to incur charges before learning that no ranking slot is available; a separate HTTP queue or larger pool adds resources without current evidence of need. This intentionally limits throughput: one slow provider request can reject every concurrent child search until it completes, is cancelled, or reaches the existing embedding request timeout (60 seconds by default). Busy rejections happen before HTTP and incur no new query embedding charge.

Callers should serialize child searches or let the parent perform one shared search. After a busy error, wait for the original request to finish rather than spinning or reissuing in parallel. After a read-only observation failure, inspect the preserved SQLite cause (file access, descriptors, locking, or query), repair it, and retry explicitly. Corrupt databases (`SQLITE_CORRUPT`, `NOTADB`) and missing tables are also observation errors; blind retries do not repair them and can repeatedly incur embedding charges after HTTP. Restore or explicitly rebuild the affected index only after diagnosing the cause; do not treat a retry as repair. The reader boundary replaces any inherited writer-only storage hint, so every SQLite read failure includes this recovery and possible-charge guidance. Cancellation and domain errors retain their existing classification and hints. Cancellation ends the observation, not its target Run. There is no new automatic retry, queue, timeout, or owner-failure condition. Revisit admission concurrency only with measured workloads and an explicit bound on extra paid HTTP requests.

## Reader lock fixture scope

The deterministic busy-timeout regression closes the WAL writer and takes `BEGIN EXCLUSIVE` in rollback-journal (`journal_mode=DELETE`) mode. It verifies classification and admission release at the actual readonly task boundary, followed by a successful writer mutation after unlocking. It does not reproduce production WAL reader contention or establish a WAL timing guarantee.

## Reader recovery guidance (Issue #27 follow-up)

The reader-local SQLite adapters preserve numeric primary and extended result
codes in the existing error details; shared writer mappings remain unchanged.
`SQLITE_BUSY`/`SQLITE_LOCKED` advise waiting for the competing operation or lock
before retrying. `SQLITE_CORRUPT`, `SQLITE_NOTADB`, schema errors, and SQL errors
such as a missing table advise inspecting and repairing the cause first. Errors
from shared ranking/provenance helpers may lack numeric codes; their hint gives
these alternatives conditionally rather than guessing by parsing error prose.
Every mapped read failure states that it is a read-only observation with no
operation receipt to query and that repeating a search after HTTP may incur
another query-embedding charge. Cancellation and domain failures retain their
existing classification. No shared error framework or owner state is added.

The rollback-journal busy fixture above verifies wait/retry guidance. A separate
non-database-file regression exercises actual `SQLITE_NOTADB`, while
the missing-table tests exercise both shared ranking and reader space-check
error paths. These are synthetic local database faults, not user-data tests.
