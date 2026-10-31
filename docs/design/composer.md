# 入力補助

採用: 2026-10-25。利用者依頼のslash候補表示と、シェル形式の上下キーによる送信履歴の呼出しを追加する。既存の下書き・非同期取得・貼付けの非実行契約を保つ。

slash名の入力中だけ、既存の全slash操作を説明付きで入力欄の上へ表示する。上下で選択、Tabで補完、Enterは未完成なら補完のみ、既に選択候補と完全一致なら既存の送信キー設定に従う。補完で操作を自動実行しない。Escで候補を閉じ、本文を変更しない。引数、複数行、貼付けたslashには候補を出さない。描画は既存Ratatuiを使用し、新しい補完エンジンや依存は追加しない。

候補を表示していないとき、入力の先頭行の上キーで現在Threadの最新の保存済みuser本文を取得する。入力の最終行の下キーは新しい本文へ進み、最新の次で元の下書きに戻す。呼出し中の上下は次の履歴を選び、編集すると通常の下書きへ戻る。これは本文の再利用で、送信・依頼編集fork・添付復元は行わない。現在の添付と編集対象を維持し、元の下書きはcursor・折りたたみ・貼付け属性も復元する。履歴のslash本文も本文として扱い、自動的な操作へ変換しない。

履歴は現在Threadの選択経路にあるuser nodeをordinalで1件ずつDB workerから取得する。画面cacheの範囲や再起動に依存せず、全履歴をRAMへ複製しない。未保存のslash操作や他Threadの本文を混ぜない。本文は切詰めず、現在のdraft byte上限を超える場合はエラーとして下書きを保つ。UIはI/Oを待たず、generation・request ID・下書きrevisionで遅い応答を拒否する。cursorはordinalとnode IDの組をlive選択経路で照合し、通常appendは許可、経路から外れたcursorは拒否して次の上キーで最新から開始する。閲覧画面のselection revisionとは独立させる。

連続上下は最大256入力のFIFOで順序を保持する。境界で上下を交互に押す場合、差分の合計だけではシェル形式のclampを再現できないため、符号付きの合計へ縮約しない。取得途中に押した送信キーも同じ順序で保留し、履歴取得後の本文を送る。応答前の編集・Thread切替・取得エラーでは保留送信を取り消す。元の下書きと呼出し本文各1件だけを保持する。読込途中のThread切替は未完了要求を無効化し、同じThreadへ戻っても新しい要求で再開できる。

検証は候補の実描画・選択と非実行、既存slash全件との照合、上下と複数行編集、元下書き復元、連続入力、遅い応答、Thread切替、Unicode・本文上限・削除済み履歴を含む。実端末は専用の合成fixtureで確認する。モデル呼出しは不要。

## 受付前の編集と操作拒否（U4/U5）

採用是正: 2026-10-25。DB受付前のAcceptingは入力欄を消してよい境界ではない。
この間の編集は表示中の本文・添付を保ったままrevisionを進める。送信snapshotは不変で、
後から来た成功receiptはそのsnapshotと同じdraft ID/revisionだけを解除する。
編集中の本文を「次の依頼との差分」と推測して削除しない。byte上限は保持中の本文と追加入力の合計で判定する。
PR1-19ではこの保全を維持し、受付成功時に異なるrevisionの入力を保持した場合は「編集中の入力を保持（次は全文送信）」と案内する。追記だけか修正かを推測して本文を切り取らず、次の明示送信は表示中の全文・添付を送る。重複を自動除去する契約は追加しない。
失敗・取消しには実際のrevisionを含む `/restore N` を示し、新しい入力を遅い失敗snapshotで上書きしない。

slashの構文不成立、UI配送の未受付、frontend準備枠の未受付、実行失敗は区別する。
非同期Command/Manageの元入力はUI内のsnapshotへ保持し、typedな送信文脈で元revisionへ結果を返す。
失敗時に消去直後のdraft全体と一致すれば元viewへ自動復元し、入力・添付が変わっていれば新しい入力を維持して復元番号を示す。Thread切替後も操作元のdraft IDだけを対象とする。
UIだけで完結する画面切替と、永続操作の受付/成功を同じ文字列prefixで判定しない。
Quitは消去後のsnapshotを既存の終了バッチへ含め、auto-save済みの `/quit` や `/qui` を正常終了後に持ち越さない。
新しいコア状態・schema・workerは追加しない。復元は送信や副作用の再実行を意味しない。

非同期の設定・添付再読込・編集原文取得・コピー・画像貼付け・snapshot復元・背景化も同じ操作文脈で扱う。
APIの成否は結果の描画処理より前に返し、既に適用された操作を描画失敗によって再実行へ誘導しない。
ポップアップを開く操作は表示開始が成功であり、その後の一覧読込みの失敗はポップアップ内の再取得導線を使う。
`/cancel` と `/cancel-wait` は呼出しをまとめる優先制御のため操作単位のreceiptを追加せず、原入力を保持する。
通常の入力編集で消せるが、自動送信はしない。UI用snapshotは既存の失敗送信snapshotと同じ保管先を使う。既存の保管期限・容量方針は変更せず、新たな永続snapshot保管や再起動後の操作復元は追加しない。
解析エラーはUI内の明示的な失敗フラグで判定し、statusの文面を受付判断へ使わない。

完全一致するslashコマンドは補完の既定候補の先頭に置く。`/cancel` のEnterが登録順で
`/cancel-wait` の補完に化けないようにする。未完成prefixのTab/Enterは候補の展開だけで実行せず、
上下キーで明示選択した候補は完全一致の既定優先より優先する。

## Saved message ownership and recovery (#52)

[Pending messages](pending-messages.md) supersedes the earlier message-send
snapshot lifetime rules above: a frozen operation-correlated submission uses a
separate existing draft row, while newer editable revisions remain independent.
The local accepting row transfers to a paired DB history/queue projection, using
operation/source-revision correlation even when Changed arrives before ACK.
Slash-operation restoration retains its existing local policy. `/restore list`
and `/restore draft ID` add durable archives; argumentless context restoration
is unchanged. Restore callbacks use composer identity/revision/generation CAS
and preserve canonical attachment positions and available frozen folds.

Normal send waits at a boundary; explicit immediate delivery uses Ctrl+Enter
unless configured as ordinary send, or Ctrl+X then Ctrl+S. Composer focus changes
reset the chord. Esc dismissal takes priority over exact-Turn interruption;
Ctrl+C remains full Run stop. The queue uses the shared popup/list/reader layout.

## Typed command prefixes with pasted arguments (#84)

A leading slash command executes only when its entire name (up to the first
whitespace) is manually entered. Pasted arguments do not disable that command.
Track only the contiguous manually entered byte prefix, not per-character
provenance for the whole draft. Insertion of pasted text truncates that prefix;
manual insertion within it extends it; deletion can shorten it but never trusts
previously pasted suffix bytes. This also permits typing a command and a space
at Home before pasted arguments. Typing only `/` before a pasted name does not
authorize it. Leading whitespace is not newly accepted.

Keep `literal_paste` and paste folds unchanged. A pasted leading slash that is
not authorized displays a composer warning and instructions to retype the name;
there is no execution/submit dialog. The warning has a named text key for future
#35 translation and uses the existing palette, without new color or marker.
The trusted-prefix counter is transient (`serde(skip)`): saved draft formats,
Core state and protocol do not change. Recall, restored source text and reloaded
drafts stay conservative; manual provenance is not reconstructed after restart.
Local snapshots retain it for rejected operations and original-draft recall.
No worker, queue, dependency, database I/O or polling is added.
