# 操作の追跡・実行情報・削除影響の照会

2026-10-25、利用者が採用した推奨三点に対するローカル拡張。既存のreceipt、再起動、Thread削除、GC境界を維持して具体化する。由来となった初期仕様資料は非同梱。実装順はoperation整合監査、既存getへの実行情報、読み取り専用の削除影響照会とする。

## 採用範囲

- operation受付と後続message／compact／索引等の完了を分ける。既存state/outcomeと同ID・同要求・同呼出元再送、異内容conflict、旧instance記録なしのexpiredを保つ。外部副作用の不明を再実行可と解釈しない。監査と是正は [operation-observability](operation-observability.md)。
- Thread／Run／Turn／messageのget、history getへ任意contextを追加する。保存された時刻・所有・状態・構造化理由だけを公開し、未知・不適用のfieldは省略する。Run/Turnのlifetime_msはcreated_atからended_atまでの閉じた壁時計時間で、準備・待ちを含む。開始時刻をcreated_atから捏造せず、全状態遷移のログ基盤を増設しない。[execution-context](execution-context.md)。
- `thread delete-preview --thread ID [--descendants]`（wire `thread.delete_preview`）を新設する。削除阻害Run、対象・子孫、添付参照、直接共有・置換出典を同snapshotで返す。容量は現GCでThread単位に安全に算定できないためnullと理由を返す。独自のGC判定器を作らず、実削除transactionの再判定を正本とする。[deletion-impact](deletion-impact.md)。

## 公開経路

CLI／共有Control／F8へ接続し、既存34操作にdelete_previewを加え35操作とする。F8の既存indexは維持して新操作を末尾へ追加する。削除影響の照会は実削除の確認画面を起動しない。既存getへcontextを任意fieldとして追加し、未指定を許容する。検索hitのprovenanceへ同じ大きい情報を重複追加しない。重い参照集計を通常のThread一覧へ載せない。

## 検証の境界

同じ操作IDの応答喪失後再送、下流処理後の再送、再起動、外部副作用の不明、compact受付の不変性を確認する。時刻の未知・不正・逆行、生成元と共有先、復旧理由と失敗理由、tombstoneを確認する。削除影響はlive/stopping、子孫の範囲、共有fork、completed置換出典、外部添付と管理参照、読取りsnapshot、既存GCと削除時の再判定を確認する。標準fmt／Clippy／全target試験と、専用合成データで実native owner・CLIの動作と再起動を確認する。利用者の稼働ownerや既定DBを試験へ使わない。変更はモデル生成／embedding HTTP adapterへ及ばないため、実APIへの有料要求を確認のためだけには追加しない。
