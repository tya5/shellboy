# 圧縮と採用

`src/compaction.rs` は通常のRust moduleとして配置する。公開入口は `Compactor::spill` と `summarize`。Controlの `/compact` は受付receiptを保存してからThreadごとの安全境界へ並べ、operation getのdownstreamで後続結果を取得する。Driverの通信・全tool結果確定と同じ境界を用いる。圧縮だけでTurnを作成・再開しない。

対象snapshotには選択revision、profile revision、reasoning epoch、経路順node ID・payload revision・実本文hashを固定する。API待機中にDB transactionを保持しない。採用時に固定版を照合し、要約node・replacement・全source参照・選択pathを同一transactionで保存する。原文nodeは変更しない。元の経路を選び直した場合は、完全一致する入力と生成規則の成功片だけを再利用する。

## Owner-scope fences during queued message acceptance (#52)

The compaction snapshot freezes both the presence and identity of its current
live Run and active Turn. Adoption rejects a newly live/stopping Run when the
snapshot had none, or a newly active Turn when the snapshot had none; existing
Run liveness and Turn generation/state fences still apply when captured.
Selected-path revision advances only on real history adoption, never merely on
message acceptance. An ordinary queued message in the same active Turn remains
off-path and cannot be erased by compaction of the unchanged selected sources;
it does not invalidate a valid snapshot solely by being accepted. This applies
to automatic as well as explicit compaction. No new counter, schema field or
worker is needed. Broad Thread metadata changes do not acquire a new compaction
fence, avoiding unnecessary discard of completed summary API work.

完結tool groupはassistantの全callと全resultを一つの境界として扱う。spillは全callと各結果のplaceholderを一つのnodeへ保存する。placeholderはcall ID、コマンド引数、終了状態、execution IDと原文node IDを含む。通常は最新完結groupを保護し、明確なcontext超過の二段階目だけ保護を解除する。reasoning epochを新しいepochへ付け替えない。

通常の要約対象は現在Turnと直近完了Turnより前の連続prefix。未反映のユーザー指示と未完結groupは対象外。64,000 Unicode文字の初期segmentを完結group境界で分け、明確なcontext超過だけ深さ8まで二分する。opaque reasoningは除外し、現在epochの送信対象推論のみを入力へ含める。部分成功はDBへ保存するが、経路採用は全成功時だけ。最終統合APIは呼ばない。

要約promptの見出しは「目的」「制約」「現在状態」「未解決・次作業」「重要参照」。見出し欠落を理由に追加APIを呼ばない。空の要約は原文を失う恐れがあるため採用を拒否する。出典IDはShellboyが付ける。

Driverは明確なcontext超過だけ、古いspill→通常retry1→最新spillと要約→通常retry2を行う。spill対象なしならretry1を省略する。通信の一時失敗はDriverの連続再試行予算で扱い、明示context超過とは混同しない。実行済みtoolは再実行しない。回数はTurn/generation単位で保持し、手動圧縮でリセットしない。

第二段階では最新spillと要約のいずれかが採用されれば通常retry2へ進む。最新spillの後に要約対象prefixがなくても、採用済みspillによる削減を無視しない（M7）。両方がno_targetなら進捗を捏造せず復旧待ちへ進む。要約の実エラーは従来どおり復旧へ返し、spill成功だけで隠さない。既存の最大二段階とtool結果の再利用を維持し、新しい状態・API・workerは追加しない。

検証: module内5試験でgroup保護・Unicode分割・旧推論/opaque除外・遅い採用拒否・実loopback HTTPからの一括採用/原文保持/完全一致キャッシュ再利用を確認。Control/Driverの操作経路・障害境界は統合試験を追加して確認する。これだけで受入全件合格とはしない。

採用するsummary/spillの各sourceに属する添付は、置換node/出典/選択pathと同じtransactionで当該Threadの送信対象から除外する。別forkを変更せず、元node/ref/外部ファイルは保持する。除外理由は `context_replacement` とし、次の通常準備でパスと理由のShellboy通知を返す。除外の後にpath採用が失敗した場合もrollbackし、原文経路だけ残って添付だけ消えた状態を作らない。追加の2 module試験で共有forkとこのrollbackを検証する。

## 受付receiptの不変性

compactの受付receiptはqueued要求として保存し、非同期成功・失敗・起動時取消で上書きしない。同ID・同要求の再送は完了後も元のqueued結果を返す。後続状態はoperation.get.downstreamのqueued/running/completed/failed、終端outcome/ended_atから観測する。再起動で取消された処理はfailed/result_uncertain=trueとし、自動で要約API等を再実行しない。過去版で上書き済みの受付を推測復元しない。結果metadataはapp_metaへ保存し既存receiptと同じ保持境界・GC論理使用量へ含める。詳しくは[operation観測](operation-observability.md)。

## 要約生成本文と監査原文（M5）

要約本文は共通Responses adapterのModelResponse.text（assistant messageの本文）だけから採る。reasoning/summary_text/reasoning_textを再帰的に集めて本文へ混ぜない。空の本文は採用しない。元sourceの参照・推論・rawは捨てず、summary.partのraw_responseへ分けて保持し、summary nodeのsummary_part_keysから監査recordを示す。このmetadataはAPIへ送るdeveloper itemに含めない。schema/table/worker/eventは増やさず、既存cacheの論理容量・source回収規則を使う。

assistant本文が空でもraw responseの監査recordは保存するが、そのrecordを成功片として再利用せず、元経路は変更しない。旧版の推論混入cacheを新しい本文規則で成功扱いしないようadapter identityを更新する。既存の不変summary nodeは書き換えない。

## suffix保護の段階解放（M2）

採用する保護suffix・分割規則を以下に示す。通常prefixから全safe prefixへ一度に広げず、保護境界を段階的に解放する。由来となった初期詳細設計は非同梱。

通常prefix、直近完了Turnを解放したprefix、必要量だけ現在Turnを解放したprefixを、同一snapshotから純粋な境界計算で求める。通常は現在Turn・直近完了Turnの最初のgroupより前だけを対象にする。明確なcontext超過時は、まず直近完了Turnの保護を外して現在Turnの最初のgroupまでを候補とする。直近完了Turnの解放で通常prefixより進まない場合は、明確なoverflowへの最小進捗として、現在Turn側の最古safe完結tool group（spill済みも同等）一つまでを起点userを含む連続prefixに加える。起点userだけの要約をtool group解放の代わりにしない。eligibleなtool groupがなければ最古whole group一つをfallbackとする。その後、残るsuffixのrender結果が既存segment基準の64,000 Unicode文字を超える場合に限り、古い現在Turn側groupから、残量が基準以下となる最小個数まで追加する。Turn IDを持たない既存spill/summaryも、現在Turnの開始以降にあるgroupとして経路順に計算する。

全段階で未完結group・未反映のaccepted/heldユーザーsource・最新完結tool groupを越えない。追加するgroupは分割せず、現在Turn側の最後のgroupを残す。最新toolの解放は既存の第二段階spillが担当し、その後のsnapshotを要約に使う。安全境界のため基準まで減らせなくても保護を越えない。

64,000文字は既存のsegment sizing指標であり、providerのcontext容量や全modelへの適合保証ではない。64K未満でもprovider容量を超える可能性があるため、直近完了Turnを解放できない場合の最小進捗は文字数に依存させない。provider容量の推測、追加の通常request、新しいstage/stateは導入しない。候補prefixの生成と要約の一括採用は一回で、既存の最大二段階の通常retryを維持する。残る容量超過は既存のrecoveryへ返す。

summary.partは従来どおり入力・生成規則ごとのcacheであり、全attemptを恒久保持するledgerではない。空本文のrawも同じ入力への明示retryが成功した際には同じcache recordで置き換わる。採用済み成功片のrawと元sourceは追跡できるが、失敗attempt全件の不変監査を新たに保証しない。

## 中断監査sourceとtool group境界（C5）

通常終了のsnapshotはshell実行中にも保存され、callとresultの間に非APIの監査sourceが入る。
そのsourceへ偽のgroup_idを付けず、同一groupの最初〜最後を含む連続範囲を一つの境界として扱う。
重なったgroup範囲はまとめ、含まれる全groupが完結した場合だけ完結とする。spill/summaryは
監査sourceを含む元の全source参照を保ち、元のcall/resultだけを途中で切り離さない。
これにより終了snapshotのselection更新があっても、既存tool結果は保存され、圧縮後もGCの
recursive source保護から監査原文へ到達できる。既存cacheはsource ID/hash列が変わるため混用しない。

範囲内の通常role付き入力はspillにも既存replayと同じ形（event_idを除去）で残す。
受理直後はreceipt保護で圧縮対象外でも、その後適用された入力の本文をsource参照だけへ落とさない。
非API監査は新たなAPI messageへ変換せず、従来のevent投影と原文参照を維持する。

## typed refusalは要約として採用しない（PR1-16）

通常会話ではResponsesのrefusal本文を従来どおり表示する。一方、要約応答のassistant
message/contentにtype=refusalが一つでもあれば、output_textとの混在でも成功片/置換として
採用しない。自然文の拒否らしさは推測せず、typedな結果だけを判定する。既存summary.partへ
raw_responseと本文を保存した後、InvalidStateと監査keyを返し、元path/sourceを保持する。
自動再生成やcontext-limit二分は行わず、再生成は明示retryに限る。

adapter v2の正当な成功cacheを一律無効化しない。保存raw_responseを共通Responses adapterで
再検証し、非空の保存textと解析textが完全一致し、typed refusalがない場合だけ再利用する。
過去の拒否cache、raw欠損、不正応答やtext不一致は成功として使わない。拒否監査も既存cacheと
同じkey/寿命で、明示retry成功時には置き換わる。採用済みの不変summary nodeの過去内容は
書き換えず、新たな失敗attempt台帳/table/workerは追加しない。

## NULL置換ノードとTurn境界（M11）

spill/summaryは複数Turnを包含しうるため、単一の`turn_id`を付与しない。通常保護と段階解放で使う境界は、現在Turn・直近完了Turnそれぞれについて、原nodeの所属から`replacement_source`を逆引きし、現在の選択経路で最も早いnodeへ投影する。Snapshotへ保持するのは最大2つの境界IDのみ。NULLの旧user nodeは保存済みreceiptのTurn割当を利用する。forkの子に祖先replacementの関連付けが複製されていなくても出典を辿り、回答だけを現在Threadの選択経路で絞る。本文やTurn所属の保存値は変更しない。

直接所属する最初のnodeより前にspill/summaryがない通常経路では逆引きを省略する。出典探索はIDだけを読み、frontierは128 IDずつ、各SQLは残予算+1行まで取得する。seedは当該Turnのnodeとreceiptから起点を選び、既存reverse indexへのEXISTSで出典参照の有無を確認する。直接Turn所属nodeと、同Turn/元Threadのreceiptに割り当てられたNULL Turnのlegacy nodeをdisjointなUNION ALLで結ぶ。legacy receiptのmessage重複だけはcovering index順でDISTINCTし、同一seedを複数回数えない。consumerは既存reverse indexを用いた探索とし、全seedのソートやrecursive CTEの無制限enqueueを置かない。一時visited/frontierも有限とし、seedおよび返されたconsumer辺のID出現数を合わせて1 Turnあたり65,536までとする（重複辺も消費する）。これは既存設定由来ではない内部安全上限であり、DB workerに任意サイズの出典閉包を持たせないための新しい失敗境界である。通常の巨大Turnは直接境界のfast pathでこの制約を受けない。走査時間の実測保証ではなく、既存index上の条件判定に必要な読み取り自体は残る。実データでこの上限へ到達する場合は、予算・index・分割探索を再検討する。

上限超過は一部の境界を採用せず`InvalidState`を返し、モデルへの要約依頼・置換採用より前に停止する。元経路を保持する。未知の出典を既知Turnへ推測して割り当てない。通常／直近完了解放／現在Turn内の段階解放、未完了group・pending user・最新tool保護は既存M2規則のままとする。

Turn境界の出典解決とその予算は要約snapshotに限る。spillは完結group・最新tool・pending保護だけを使い、Turn prefixを参照しないため、同じ原経路読取りを使っても出典閉包は解決しない。これにより要約出典が予算を超えるThreadでも、古い完結groupの通常spillを妨げない。要約の上限超過時に元経路を保持する契約は維持する。

## Cache/adopt境界の局所整理（R2-8）

R2-2/PR1-16のCachedPart load/encode/reusableは再利用し、saveを同型に集める。大きいraw JSONのencodeはDB job外、raw監査保存はrefusal/空本文の拒否より前、再利用判定と明示再試行時の課金境界は変更しない。SummaryAdoptionは既存snapshot/sources/payload/count/resultの所有を明示するprivate・一回限りの採用値で、8つのclosure captureをadoption/cancelの2つへまとめる。既存Vecを移すだけで複製/新queue/worker/永続状態を増やさない。取消し→snapshot validate→replacement/source/添付除外→path CAS→commitの順は同一transaction内に維持する。


## Turn出典seedの対象限定走査（PR2-15）

旧seed SQLはreplacement_source全体から所属を判定しており、LIMITは返却件数だけを制限して
無関係なreplacementの走査を制限できなかった。migration9で`node_turn(turn_id,id)`と
`receipt_turn_source(turn_id,target_thread_id,message_node_id)`のcovering index2本を追加し、
当該Turn候補へのseekを起点にする。node枝はid一意、legacy枝はNULL Turnだけなので互いに
重ならない。legacy receiptが複数あっても一つのmessageをseedとして一度だけ返す。
replacement_sourceの複数fanoutはEXISTSでseedを増やさず、後段consumer辺の重複出現は従来どおり
一件ずつ65,536のgraph work予算を消費する。fork/shared出典の探索を現在Threadだけで絞らない。

利益は無関係なreplacement数に比例するseed走査の除去で、関連Turn自身のcandidate数やconsumer辺数、
DB待ち時間の絶対上限ではない。費用はnode/receiptの各rowにつきIDを含むindex entry一つずつと、
INSERT/UPDATE/DELETE時のSQLite index保守。初回migrationだけ既存rowから構築し、通常起動/要求時には
再構築しない。新表・column・event・worker・graph engineは追加しない。既存migrationは改変せず
新migrationとledger/user_versionを原子的に確定する。本文・receipt割当・source順序は変更しない。

回帰は配布bundled SQLite上で旧SQLとのseed集合一致を直接確認し、無関係node100→10,000・各3参照で
StatementStatus::VmStep/FullscanStepを比較する。時刻の偶然に依存せず、旧SQLの増加と新SQLの一定性、
covering index seek・sortなし、duplicate receiptとfanoutの扱いを検査する。既存のnested summary/spill、
legacy/shared境界、正確な小予算/production65,536境界で、超過時には要約HTTP前にInvalidStateとなり
元経路を維持する条件も保つ。LIMITを走査時間上限とは称さない。


## 範囲選択と編集の共通化（Issue #16〜#18）

`compaction/policy.rs` はsnapshotだけから完結groupの添字範囲を返す純粋な規則を持つ。旧手動compactとcontext超過2段階の選択規則を保ち、spill・summaryの採用は従来どおり別transactionとする。`compaction/range.rs` は全編集に共通する境界検証を担当する。未完結group・実行中call・保護node、group途中のnode ID、逆順・重複範囲を拒否し、黙って丸めない。エラーには近傍16組までの使用可能な始終node候補を返す。

部品は、指定範囲のspill、summary、決定文への置換（omit）の3種類。prefixの省略も途中の省略も同じomit採用を使う。前後の経路を保持し、置換・出典・添付除外と`select_path`を既存の短いtransactionへまとめる。新しいtable・worker・queue・event・依存は追加しない。編集は既存Thread境界へ並べ、進行中の通信やtoolを遡って変更せず、次の要求から適用する。

### Failure-driven maximum reduction (#50)

Adopt [failure-driven reduction](failure-driven-reduction.md). No token estimate,
75k activation or compact_hint_tokens remains. The first eligible transient
failure retries unchanged, the second and later eligible failure may reduce;
explicit context overflow reduces immediately. All resends share the configured
consecutive retry budget (default eight), including after reduction. A successful
response resets that budget and the explicit-overflow stages. Safe candidate
absence retries unchanged for transient failures. Explicit capacity rejection
with no adopted reduction recovers immediately; no independent reduction
allowance is renewed or spent.

Maximum omission retains the current user request, latest group, latest complete
tool group, incomplete work and protected/unapplied instructions. Existing bounded
provenance, atomic adoption and attachment exclusions/restore rules remain. An
edit that cannot reduce projected characters is a no-target result. It never
resends tools or calls a summary API. Only the existing second explicit-overflow
stage may spill the latest group and summarize. /compact-max and compact --maximal
expose the API-free action without node IDs, independently of retry.

ContextEditResult reports before_characters, after_characters and
changed_suffix_characters: exact Unicode scalar counts of replay JSON, not token
or cache-hit estimates. Original sources and side effects remain; restore can
return omitted text without reactivating excluded attachments. Size facts do not
predict provider acceptance. Stop/selection/profile/generation fences are unchanged.

### 明示編集と復元

- `compact --spill --from NODE --to NODE` / `--summary`：inclusiveな完結groupの始終ID。明示spillは最新完結tool groupも選べる。
- `compact --omit --from NODE --to NODE`：prefixまたは途中範囲を通知一つへ置換する。現在の依頼を含む指定は拒否する。
- `compact --trim --to NODE`：先頭から指定したnodeまで（inclusive）のomit。
- `compact --restore [--from MARKER]`：選択経路上の指定marker（省略時は最後に作成されたもの）を原文へ戻す。TUIの引数なし`/restore`も同じ操作。既存`/restore 番号`は失敗下書きの復元として維持する。

受付は従来compactと同じ不変receiptとdownstreamを使う。引数はreceiptの`context_edit`へ保存し、同じThreadの明示編集を順に独立実行する。連続する引数なしcompactは従来どおり一回へまとめ、明示編集を越えて結合しない。新しい永続キューは作らない。引数なしcompactは旧規則のまま。結果には編集前後・目標・無効になるキャッシュsuffixの推定tokens、元node件数、marker ID、警告を含める。

復元は現在のmarker位置へ元の順序を挿入する。省略時の経路全体を再採用せず、後から追加されたnodeを保持する。自動縮小で途中に現在の依頼を残した場合は、markerの元順序と保持anchorを照合して重複を除く。曖昧・失われたanchor、遅いselection/profile/generation変更や停止は拒否する。省略の監査とGCの根は`thread_replacement`へ残す。添付はexcludedを維持し、復帰が必要なら新しい添付IDで明示的に追加する。

省略元の始終IDでsummaryを要求したときは、元データを内部で読み、既存の分割・キャッシュ・raw監査・拒否判定を使ってmarker位置へ要約を採用する。モデルへrawを読み直させない。最初の実装では一つのmarkerの省略元全体を対象とし、部分的な隠れた範囲は使用可能な全体境界を示して拒否する。`--focus`、終点の巻き戻しは追加しない。

復旧・動作中のTUI概要は、同じTurn/generationの直近256 attemptから縮小の数値だけを投影する。取得範囲外は「縮小なし」と断定せず概要を表示しない。markerの原文・元attemptの記録は別途参照できる。巨大なerror JSON全体をUIへ渡さず、新しい追跡tableやindexは追加しない。

Omission sources and retained anchors together are bounded to 65,536 nodes,
matching the restore provenance budget. Oversized manual edits are rejected
without changes and suggest a smaller range; automatic edits adopt the bounded
oldest whole-group window described above. The automatic planner is a pure
function without DB/HTTP access, counts each group's projection once and
estimates the remaining size by subtraction without cloning all history for
each candidate.

サイズ補正は同Thread・接続・モデルの直近64成功attemptを有限に調べ、有効なusageと入力文字数がある最新のものを使う。送信manifestの`history_characters`で固定指示・添付等の差を分離し、同selectionのinflight記録があればその差を優先する。過去の添付projection等を固定分として推定するため、省略による添付除外後の減少を保守的に見積もることがある。`context_usage`は補正元attempt ID・方法・測定できていない対象を返す。

構造分離の測定（`7dfc96d`→`5ee28ba`、関数の宣言から閉じ括弧まで）: `spill_cancellable`は125→10行、`summarize`は150→15行。旧範囲の公開選択入口は純粋な`spill_ranges`/`summary_range`の2か所へ分離した。完結/保護の判定は`range::group_error`の一か所を規則と共通validatorから利用する。始終IDの組境界変換と採用時CASはそれぞれ別責務として残し、同一規則を二重に実装しない。


## レビューによる通知と診断の精度改善

現在依頼より後ろの作業も省略した場合、markerの`after_current_request_nodes`と本文にその件数を明示し、実行済み副作用を確認してから再実行するよう案内する。手動の途中省略を単なる「古い履歴」と呼ばない。markerの添付件数と結果の警告は実際にactiveからexcludedへ変わる参照を対象とする。

`--restore --from ID`は、選択中markerのIDを優先し、一致しなければ省略元の最初のnode IDを一意に解決する。通知はその元IDを指定した復元コマンドを示す。候補が複数なら曖昧な復元をせず拒否する。

自動縮小が採用されなかった場合も、元の復旧遷移と同じ保存経路に`automatic_trim_attempt`を記録する。起動閾値未満と安全な縮小では目標に届かない状態、競合等による拒否を区別し、成功採用の`automatic_trim`やTUIの「縮小済み」と混同しない。

Transport failures before any HTTP-phase observation (including client-pool
construction failure) keep ordinary retry and do not activate size reduction.
Missing observation is not evidence of a partial upload or an oversized request.
