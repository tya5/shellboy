# UIの停止・終了予約

採用是正: U1/B05、2026-10-25。UI/frontend内の配送を分け、コア状態・DB・event・依存は追加しない。
通常操作は既存の有限FIFO、停止はThread別最大32件の重複統合、Quitと終了選択は固定slotを使う。
UIは同期I/O・キュー空き待ちをせずtry操作だけを行う。receiver消滅や停止対象の上限時は未受付を返す。
短いメモリ内のmailbox更新以外をlock中に行わない。

Quitの受理時に通常FIFOを閉じ、既に受理したSend/SendEdited/SaveDraftを順に取り出す。
管理照会等を新たに開始せず、終了を任意の長時間管理操作の後ろへ置かない。
UIのSaveDraft＋Quitは1バッチとして扱い、最新の保存snapshotをQuitの固定slotへ同梱する。
重複Quitで最初のsnapshotを上書きしない。普通のFIFOが満杯でも保存snapshotを失わない。

終了段階はUI/IPC受付凍結→受理済みの有限なUI送信準備・保存のdrain→Control停止とする。
この保存前段階では既に所有する処理が継続し得る。新しい外部受付は許さず、保存済み受付と取り違えない。
終了futureは既存IPCの受付を閉じ、既受理の送信準備・保存をdrainした後でControlの停止へ進む。
その全体を既存のshutdown grace/Wait/Force選択で観測する。送信がDB受付を得る前にControlを停止して
受理済みUI本文を破棄する順序にはしない。保存queue・managed画像pin・DB close/owner lockの寿命は維持する。
通常の管理結果は待たず、停止で観測を解除する。外部から既にDB受付済みの操作の保証は変更しない。

Cancelは通常command_slotsを使わず、既存frontend JoinSet内の専用同時実行1枠で処理する。
停止対象は要求時のThread ID。完了まで同じ対象をmailbox内で重複統合する。
別Threadの上限超過は明示未受付とし、有限性のために勝手に対象を置き換えない。
人間UIのCancelは内部adapter `Control::cancel_thread` → `Store::stop_current_thread` で、
同じDB transaction内でThreadの生存Runを解決し、既存の `stop_run` reducerを呼ぶ。
通常のThreadGet＋RunStop requestにしないため、receipt予算満杯でも停止できる。
UI actionは公開operation IDを持たず、CLIの冪等再送保証を偽装しない。公開CLI/wireのreceipt契約は変更しない。
既存のadmit・保存失敗判定・apply_effectsを再利用し、内部method2個以外のコア機構は追加しない。
終了猶予中のWait/Forceも通常FIFOを迂回する。Forceが利用可能になる時点と既存F→Y確認は変えない。

検証は通常FIFO飽和、command_slots飽和、重複/対象上限/切断、終了snapshotと先行保存・送信の耐久性を
一時DBとmockで確認する。実端末I/Oや通常のUI拒否時のquitting復帰とは別の配送境界である。
