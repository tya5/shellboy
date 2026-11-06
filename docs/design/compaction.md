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

完結tool groupはassistantの全callと全resultを一つの境界として扱う。spillは全callと各結果のplaceholderを一つのnodeへ保存する。placeholderはcall ID、コマンド引数、終了状態、execution IDと原文node IDを含む。通常は最新完結groupを保護し、明確なcontext超過の二段階目だけ保護を解除する。推論の扱いは [#182](#signed-reasoning-and-history-edits-182) の位置規則に従う（編集位置より前は保持、後ろは送らない）。

通常の要約対象は現在Turnと直近完了Turnより前の連続prefix。未反映のユーザー指示と未完結groupは対象外。64,000 Unicode文字（既定。#231で設定可）の初期segmentを完結group境界で分け、明確なcontext超過だけ深さ8まで二分する。opaque reasoningは除外し、現在epochの送信対象推論のみを入力へ含める。部分成功はDBへ保存するが、経路採用は全成功時だけ。最終統合APIは呼ばない。

Summary instructions always use English headings: Goal, Constraints, Current state, Open issues and next steps, and Key references, regardless of UI language. Missing headings do not trigger another API call. Empty summaries are rejected to avoid losing original content. Shellboy supplies source IDs. Prompt version v2 separates new summary-cache entries from v1; saved summaries remain verbatim and are not migrated or translated.

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

64,000文字（既定）は既存のsegment sizing指標であり、providerのcontext容量や全modelへの適合保証ではない。64K未満でもprovider容量を超える可能性があるため、直近完了Turnを解放できない場合の最小進捗は文字数に依存させない。provider容量の推測、追加の通常request、新しいstage/stateは導入しない。候補prefixの生成と要約の一括採用は一回で、既存の最大二段階の通常retryを維持する。残る容量超過は既存のrecoveryへ返す。

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

Turn inputs are never omitted (#185): the automatic maximum omission keeps
every user message of the active Turn verbatim, i.e. the request and every
message delivered later in the same Turn (including leader messages), as
retained anchors inside the single omission marker. The marker notice states
this, and only work between those anchors is omitted. Before this rule, a
2026-11-02 overflow recovery reduced an 11.7M-character Turn to 19K characters
and dropped four mid-Turn leader instructions. The operation stays API-free; if
the retained inputs alone still exceed the provider limit, the existing
recovery wait applies rather than dropping them. No setting.

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


## Retrieval guidance after omission (#71)

Omission notices keep the existing keyword history-scope RAG command. If and
only if both embedding.connection and embedding.model are configured, also
suggest `shellboy history search --thread THREAD_ID --query TEXT` for decisions
and context with different wording, and disclose the query-embedding API cost.
This is guidance only: do not search, embed, create an index, inspect credentials,
or assume index coverage from configuration. With no embedding pair, the notice
remains keyword-only. Keep source IDs, restoration/summary commands, side-effect
warnings and immutable provenance unchanged. Agent guidance also explains
cross-conversation semantic retrieval and verification of historical evidence;
usage observation is a manual read of existing records, not new telemetry.

## Bounded explicit summaries (#73 / D-015)

Explicit range summaries use map/reduce, not concatenated part summaries. Keep
map inputs chronological and group-atomic; pack complete chronological map
parts, including their source-ID lines, into reduce batches of at most
`segment_chars` (default 64,000) characters, then consolidate again until one final summary remains. Never split
a part: an oversized part is sent alone and counted in the existing summary
usage record as `oversized_reduce_parts` without storing its contents.

Prompts deduplicate goals/constraints, keep only the latest confirmed state,
retain decisions/reasons, unresolved work, uncertainty and source node IDs.
Original payloads, replacement sources, cancellation and transactional CAS
adoption remain unchanged. Per D-017, automatic stage 2 shares these bounds.

Summary parts are cache records in `app_meta` (`summary.part.<digest>`); the key
has no Thread ID. Each record stores `text`, `sources`, `cache_key`, the
normalized `raw_response`, and (#232) `stage` (`map`, or `reduce` with `round`).
A reduce part's `sources` is the ordered, de-duplicated union of its input
parts' sources. The summary node's out-of-band payload keeps `summary_part_keys`
(map) and `summary_reduce_keys` (every reduce round, in creation order); replay
sends only `item`, so neither reaches a request. `shellboy history get --node
SUMMARY --parts` is the read-only reader (cli-reference): it reports absent
fields of older records as null, reports a missing record as `not_retained`,
and never reconstructs. A retry with the same key overwrites the part, so the
reader shows the current record. A fork shares node rows and copies the model
settings, so it computes the same map keys for a shared prefix and reuses the
parent's parts (tested, S2). Parts are never nodes and are not indexed.

`[compaction] summary_max_chars` and `summary_max_percent` select the smaller
limit, floored at 2,000 Unicode characters. An absent section defaults to 20,000
and 5%; a present section may specify either or both (not neither). Percent is
finite, greater than zero and at most 100; character limits must be positive.
Explicit `--max-chars`/`--max-percent` replace their respective configured values.
Source size is the selected original replay projection, including when a marker
is virtually expanded. Count final summary text, not retained out-of-band source
metadata. Reject a summary exceeding its limit or failing to shrink its original
source projection; omitted information can still enlarge the currently selected
marker and must be reported as such. Never truncate a model answer to pass.

Use independent configured map/reduce request limits (default 128/16) and at
most four reduce rounds. Preflight uncached map segments before HTTP; cached maps
do not consume map budget. Map prompt/key identity remains pre-#73 and independent
of size policy; reduce keys include the output policy, model/profile/effort/epoch
and input identity. Existing summary.part metadata and raw validation are reused.
No new table, worker, queue, timer or automatic transport retry is added. Refusal
or empty responses remain auditable and cannot be adopted. If budgets/limits
fail, preserve the selected path and return source/summary/limit/request facts.
Successful results expose those facts too. Omission notices recommend leaving
history omitted and bounded history search/get first; a capped summary explicitly
reintroduces key facts rather than undoing the side effects of earlier work.


### D-017 review correction: all summary paths share bounds

This supersedes #73's initial shared 64-call budget and automatic-path exception.
All summaries, including #50 stage 2 and argument-free compact, use the same
reduce and size rejection before adoption. Preserve the pre-#73 SUMMARY_PROMPT,
summary_key and raw-validated map cache identity; output limits affect only reduce
prompts/cache keys. Map/reduce request ceilings are independent, default 128/16,
configurable via summary_map_max_requests/summary_reduce_max_requests (1..4096).
Before HTTP, count all known uncached segments and reject an excessive map plan
with total/cached/uncached counts, limit and narrowing/configuration guidance.
Cached maps do not consume budget. Context-limit split attempts remain charged
to the map budget because their necessity is learned only from the provider.
The four-round reduce bound remains. Existing #50 rejection handling, cancellation,
provenance and atomic adoption remain unchanged; an earlier spill may be saved.
Omission notices do not hardcode size flags: ordinary summary uses user settings.

### Segment size and reduce rounds (#231)

`segment_chars` (default 64,000, accepted 2,000..=1,000,000 Unicode characters) is the
map-part size and the reduce packing size, formerly the constant `SEGMENT_CHARS`.
`reduce_rounds` (default 4, accepted 1..=16) is the former constant bound on reduce
rounds. Both resolve like `auto_at_tokens`: `[compaction]` default, then
`connections.<name>` override, then the per-Thread override
(`thread.segment_chars` / `thread.reduce_rounds`, nullable integers, migration 31, set by
`thread set --segment-chars N|''` / `--reduce-rounds N|''`, copied by child Threads and
forks, kept across connection changes), then the per-invocation
`compact --summary --segment-chars N --reduce-rounds N` (the call's own value, summary
mode only; not accepted by the argument-free default compaction or its preview, which use
the resolved values). The snapshot resolves the Thread/connection/default levels once
(`Snapshot.segment_chars` / `reduce_rounds`), so the argument-free `compact`, the
proactive path and `compact --dry-run` see the same values; the bound also sets the
overflow-prefix baseline.

Part identity: a map part's key is computed from the profile/model/effort/epoch and
the covered nodes (their revisions and hashes), not from the size. A different size
moves the part boundaries, so a part covering different nodes has a different key and
the parts of another size are neither reused nor overwritten. A part covering the same
nodes keeps its key and is reused. Reduce keys hash their input text and are unaffected
by size except through that text. Unit test: `part_keys_follow_segment_boundaries...`.

Recorded values: every compaction result and automatic stage record carries
`segment_chars` and `reduce_rounds` (also in budget/limit failure details), next to the
existing `summary_limit`; `compact list` copies them into `metrics` (unsigned integers
only; missing in records before migration 31 means unknown). For an automatic event
`trigger.auto_at_tokens` keeps the threshold that fired. Dry-run `summary` reports the
values it used (`segment_chars`, `reduce_rounds`) with `segments` / `cached_segments` /
`uncached_segments`. The agent can therefore compare results under their own values
without a settings-history table (the values belong to the result, not the Thread).
Numbers are limits, not guidance: no recommended value is shipped.

## Read-only compaction forecast (#113)

`compact --dry-run` selects `compact.preview`, a domain-specific read-only command
with no receipt, queue admission, model/embedding request, marker/spill/part write
or path adoption. It supports default spill-then-summary and explicit summary,
spill, trim, omit and maximal; list/restore combinations are rejected. Existing
mutation commands and safe-boundary admission are unchanged. Preview uses the
existing finite presentation pool and one query-only WAL snapshot, never DbWorker
or the UI thread. No table, worker, queue, polling or generic planner is added.

Use canonical grouping, range/protection policies, spill payload, trim marker,
replay character measurement, summary segment preflight and successful-part
validation. A virtual spill precedes default summary, as in real execution.
Summary text and its final length are unknown: return null after-size plus the
source size and configured bound, not fabricated text. A deterministic failure
is returned as forecast facts (code/message/hint), not successful empty targets;
limits are unavailable forecasts, never predictions that an operation will fail.
Completed deterministic before/after sizes count replay JSON Unicode characters,
not SQLite bytes, tokenizer values, fixed instructions or terminal wrapped lines.
Affected/removed user/assistant message counts refer to replay message records.
A forecast is not a reservation; actual execution still revalidates its fences.

The collection envelope is 4096 combined selected/hidden nodes, 512 Thread Turns,
4096 receipts, 4096 tool-call/attachment/ancestry rows each, 16 ancestry layers,
1MiB per payload/cache entry and 8MiB copied text total. Scalars are checked before
copy (IDs1024, kinds/states64, paths128KiB). Every query uses literal Thread or
exact-ID indexes with LIMIT+1; overflow yields InputLimit with unknown coverage.
Backward replacement ancestry from selected roots avoids scanning lifetime Turn
seeds; edges/cycles/depth are bounded, never guessed. At most28 SQL statements
(10 collection/preflight/cache plus16 ancestry layers and two hidden-source reads)
are allowed; counts and indexed plans are tested, not timing thresholds. This
counts the collection itself; connection query_only and BEGIN/COMMIT add three
fixed statements outside that counter.

Original context markers are rendered via #112's short outbound notice, excluding
provenance arrays. Explicit summary of one marker/full omitted-source range uses
its checked saved replacement sources, matching canonical reintroduction behavior;
a genuinely oversized visible indivisible group forecasts the canonical refusal.
Default spill's newly-created replacement IDs cannot hit prior summary caches;
other segments use existing cache validity. Provider/model failures after sending
and races after the snapshot remain unpredicted. Final output is prepared after
the read snapshot closes, with current capture revisions and explicit limits.

## Proactive threshold compaction (#125 / D-019)

Positioning: in normal operation the conversation stays at a low token count.
A momentarily huge input is never blocked or pre-checked; it is sent. Only after
its response reports input tokens at or above the threshold does the Driver
start proactive compaction, at the next safe request boundary (one step late is
accepted). A measured #209 target shortfall can continue at a subsequent safe
boundary below the threshold. The
existing explicit-overflow reduction (#50) remains the last-resort fallback, so
the conversation always continues. Users who want a persistently large context
opt out with `auto_at_tokens = 0`.

Setting: `[compaction] auto_at_tokens` (default 200,000; 0 disables) with an
optional `connections.<name>.auto_at_tokens` override (the connection value wins,
0 disables for that connection). A section containing only this key keeps the
default summary limits.

Thread override (#229): `thread.auto_compact_at` (nullable integer, migration 30) is set by
`thread set --auto-compact-at N|0|''` (N sets, 0 disables for the Thread, `''` clears) and
wins over the connection and `[compaction]` values (`Compaction::auto_at_tokens_for`). The
driver reads it at each request-boundary check, so a change applies from the next check and
never to an in-flight request.
Children and forks copy it at creation; a connection change keeps it; `compact list`
`trigger.auto_at_tokens` stays the value that fired. `thread get` returns `auto_compact_at`
(null if none) and `effective_auto_compact_at`; the TUI `/200K` shows the effective value.
No restriction: the Thread's model may change its own value like other `thread set` fields.

Trigger: only the provider-reported `input_tokens` of the Thread's latest
completed attempt for its current connection/model, read with the same query as
`context_usage.last_successful_attempt`. No character or token estimate is used.
Missing/unknown usage never triggers. Usage measured before a later adopted
replacement (`next_request_pending`) is stale and never triggers, so a successful
compaction is not repeated until a new request reports usage again.

Boundary and path: in the Driver loop after the next Turn snapshot is ready, the
Thread boundary guard is held, all tool groups are complete and no request is in
flight, i.e. the same boundary as queued `/compact`. The Driver releases its
replay lease and runs the argument-free compact unchanged (spill, then the bounded
summary with the ordinary protections); never maximal omission. Afterwards it
re-reads the snapshot. Candidate forecasting uses the existing bounded read-only
preview loader and presentation workers; no table, queue, timer or HTTP stage is
added.

Partial spill adoption (#153 review): proactive compaction spills only whole safe
eligible tool groups whose exact replay JSON character projection shrinks; this
is a local reduction check, never a token estimate or trigger. The latest group,
incomplete groups and protected nodes remain fenced. Spills commit before the
summary request. If summary subsequently fails for a non-storage reason, an
already committed spill is recorded as `adopted` with `summary_rejected: true`,
the original summary error, its provider-reported summary usage and the spill
counts. This includes cancellation of a following closed- or active-Turn summary:
record `cancelled: true`, the cancelled summary stage and original summary error,
while retaining the exact committed source IDs/counts. Save the partial-adoption
guard and calibrated pending target for the next provider measurement before
returning that original cancellation error; cancellation never means the prior
spill was rolled back. No spill plus summary failure remains `rejected`. Storage
failures propagate. Manual compact keeps its existing contract. Replay character
reduction is not a guarantee of provider token reduction.

Progress gate (#180): crossing the provider threshold, or verifying a #209
target shortfall, is necessary but not sufficient. Before starting, a bounded read-only forecast applies the same
ordinary Turn/tool fences, protection checks and reducing-spill projection. If
there are no eligible sources, no compaction is started, no summary request is
sent and no `no_safe_reduction` attempt is written. If an attempt or its summary
is rejected or makes no progress, automatic work is not retried solely because
reported tokens or protected current-Turn text grew. It is reconsidered only
when saved source evidence permits a new older summary source, a larger
previously eligible source, or an older complete tool group that can reduce.
Summary and spill evidence are tracked separately: a new spill opportunity does
not reissue a rejected summary for the same older sources, and a new summary
opportunity does not repeat an unchanged spill pass. If a partial spill changes
the selected path while coverage remains unknown, its exact committed selection
revision permits the guard to refresh its own witness without treating that
replacement as new input. When the newest tool anchor belongs to the current
protected Turn, later appended groups there do not unfreeze that rejected older
summary; an intervening path rewrite or new Turn is evaluated again. The latest tool group, every active-Turn user input, the latest completed
Turn, pending receipts and held deliveries retain their existing protections.
The active-Turn summary remains eligible only through the #182 plan (all tool
rounds of the Turn); its evidence is separate from the older-Turn summary
evidence. While the tool loop is open, spill opportunities are left out of the
guard (deferred, not seen), so the closed-loop boundary still treats them as
new (#182). Explicit compact and
failure-driven overflow recovery use their original separate policies.

After a no-target decision or no-progress attempt, one bounded `app_meta` guard
per Thread stores the prior selected-path witness, a hash of older complete tool
frames, anonymous source-identity hashes, maximum model-visible character counts
per spill/summary opportunity, fixed reason and saturating rejection counters.
It stores no prompts, outputs or provider error text. Source history is capped
at 8,192 marks and encoded state at 1 MiB; Thread deletion removes it and its
small status record. A successful reduction clears the guard. Like other
internal `compact.automatic_*` metadata, these keys are not included in the
#171 `logical.history` counter: the cap is per Thread, not a global disk quota,
so many blocked Threads can accumulate additional disk usage. This accounting
boundary remains under review. The fast path
compares metadata already present in the replay lease and does not read model
payloads again. A path rewrite, new Turn or newly older tool group triggers a
fresh bounded forecast; identical or smaller prior candidates stay suppressed.
When a complete forecast identifies receipt/delivery-protected sources, the
bounded forecast is repeated to observe a possible protection release, but an
unchanged candidate set neither starts compaction nor emits another skip
diagnostic. A coverage-limited forecast cannot currently observe a protection
state change that leaves the selected path and tool frontier unchanged; detecting
that case without an unbounded scan remains an open limitation.
The existing preview bounds (4,096 nodes/rows/turns, 8 MiB copied text, 1 MiB
per payload, eight ancestry levels and 128 SQL statements) apply. If a protected
payload alone exceeds those bounds, an additional metadata-only proof can still
establish that the first selected source is behind the current/latest-complete
Turn fence, cached replay proves that older tool groups cannot reduce, and no
active-Turn middle tool group or spill marker could be summarized. This
proof reads no saved payload from SQLite; it examines at most 8 MiB of already
replayed JSON and 4,096 frames when multiple tool groups exist. A direct first
source/Turn check also covers longer paths; ancestry expansion visits at most
4,096 path nodes and 512 Turns. The proof interrupts after at most 131,000 SQLite
VM instructions. Interruption or an
inconclusive lower bound discards the proof. If coverage
remains incomplete, it is explicitly unknown, never reported as an empty set;
the first attempt, or a subsequent changed-path/Turn/tool frontier, may proceed
conservatively. An unchanged frontier remains suppressed. This bounded fallback
can reevaluate a changed but non-growing candidate set when exact evidence is
unavailable; it does not infer eligibility from token growth.

The existing activity footer shows a localized deferred reason, never an active
compaction spinner, and only for the corresponding live Run and Turn. Fixed,
content-free diagnostic events distinguish measured below-threshold, no target,
no new target and rejected `invalid_state`/other outcomes. Rejection counters
saturate rather than grow
without bound. The request continues normally; no-target and non-storage
rejections are not Turn failures. Storage failures still propagate. No new
configuration or retry interval is introduced.

Ordinary summary scope is intentionally narrower than the input: it ends before
the current Turn and latest completed Turn, or an earlier protected/incomplete
group or latest complete tool group. Consequently, during a long Turn it may
only re-summarize a small older prefix (including earlier summary markers),
while the bulk of the reported input is outside that prefix. Current-Turn old
complete tool groups can be spilled, excluding the latest group, but even their
spill markers remain outside ordinary summary scope. Spill omits tool outputs,
not all model output or current-task text. This boundary can explain #153's small
summary source despite large input; the particular private path was not inspected.
Unspilled tool output is not categorically excluded from an eligible summary
range. Retries do not relax
these protections or promise that every long Turn can be summarized. Explicit
overflow recovery remains a separate, wider policy.

Record: `automatic_trim_attempt` on the triggering completed attempt (existing
field and `automatic_compaction_turn` partial index) with `reason =
"proactive_threshold"`, `outcome`, `summary`/`error` like stage 2, and
`trigger.auto_at_tokens` / `trigger.input_tokens` / `trigger.run_id`. `compact
list` shows it as `automatic_reduction` with `metrics.reason` mirrored in the
existing top-level `reason` (`failure` for existing failure-driven records) and
those trigger values. The bounded attempt reason already read by the projection
is reused; no extra source lookup or raw error parsing is introduced (#132).
A second Run triggered by the
same measurement overwrites the earlier record on that attempt.

## Active-Turn middle summary (#181, reshaped by #182)

Background and the GPT-route evidence: [midturn-compaction](midturn-compaction.md).
The #182 shape below supersedes its sections 1-4 and 7.

Order (user decision 2026-11-03, reverses #181's "instead of"): in the #125
proactive attempt the ordinary summary of earlier Turns runs first, as usual,
because recent context contributes more to quality. The active-Turn summary
runs when the prefix holds nothing but earlier summaries, or when a previous
prefix summary remains above the measured #209 target. With a calibrated plan,
a prefix and an active-Turn summary may also run in the same attempt if their
measured replay-character reduction is short of the planned reduction. The
#180 progress gate and the fresh-measurement rule remain in force. The attempt record has
`midturn: {"skipped":"prefix_first"}` when the prefix summary ran,
`{"scope":"active_turn"}` when the active-Turn summary ran,
`{"skipped":"no_target"}` when it had no target and `{"skipped":"no_new_target"}`
when the progress gate saw no new active-Turn source.

Shape (`policy::active_turn_plan`, pure): ① earlier Turns and ② the active
Turn's request, plus anything before its first tool round, stay byte-identical.
All tool rounds of the active Turn, from the first tool round (tool group,
spill, earlier summary or trim marker) through the latest complete tool group,
are replaced by one developer summary: ③ the older rounds are summarized with
the existing bounded map/reduce, and ④ the latest complete tool group is
appended to the same item as plain text (calls and results; reasoning and
signed or opaque provider blocks are never included). No thinking, tool call
or tool result of the active Turn remains after the edit point. User decision B
(2026-11-03, simplified): the active Turn's user messages inside the span leave
the selected path with the tool rounds, so the path is `[earlier Turns]
[request][summary]`, the shape of every summary, and no original node gains an
edge (only request -> summary is added). Shellboy does not quote them: the
summarizer input is the span ③ in its original order with the messages
interleaved where they occurred, and the summary prompt asks for a final
"User instructions to keep" section listing what must stay in effect
(constraints, exact values, changes of direction) with node IDs; meaning, not
byte-exactness, is the requirement. The node content is the notice, the
model's summary, then ④ rendered as text by code. One summary request
(map/reduce bounds unchanged). Messages after ④ stay on the path. A span
holding a protected node (pending receipt or revisit event) is not a target.
The notice says how many messages were summarized, that their order relative
to the steps may differ, and to follow the originals with `shellboy history
relations --node FIRST --relation next` when exact order matters.

General rule for every summary (ordinary, active-Turn, nested): the
"User instructions to keep" section is part of every summary prompt (map and
reduce, prompt v3), original nodes and edges are never modified, and the
message IDs a summary covers (a message ID is its message node ID) are derived
on demand from the ordered replacement sources by `history::summary_message_ids`
(nested summaries expanded through their own sources, at most 8 levels and
65,536 source rows; the `replacement_output_node` and replacement-source
primary-key indexes keep it bounded). Nothing extra is stored on summary nodes.
`history relations --relation sources` exposes the same ordered sources.
Reply-expected messages need nothing in summaries: #194 finds them through
`thread get awaiting_reply` and the Run-end reminder. No target without a current Turn, with an
incomplete group in the span, or when ③ has no tool group or spill marker (an
earlier summary alone never re-fires, #180).

Summary: the existing limits, refusal/empty/over-limit/shrink rejections and
cache keys; covered sources are ③ and ④ minus retained nodes. The fixed notice
says the covered tools already ran and the latest round follows as text. The
adoption keeps no later assistant output, so the reasoning epoch is unchanged
and earlier reasoning keeps replaying (the #181 epoch advance is removed).
Manual argument-free `/compact` skips its spill while the tool loop is open,
then runs the ordinary summary and, only while the loop is open and that has no
target, the active-Turn summary.

## Signed reasoning and history edits (#182)

Claude 5.1+ binds each signed thinking block to the exact request prefix before
it; a client-side edit before a retained block makes the provider reject it.
The same rule applies to every adapter (no per-provider branch).

1. History is edited only while the next request is prepared, never between a
   tool call and its result (existing Thread boundary).
2. Positional rule for every replacement (ordinary summary, active-Turn
   summary, trims, spills, omission, restore): reasoning strictly before the
   earliest edited position P stays byte-identical; reasoning saved at or after
   P is never sent again. `history::select_path`, the single choke point, finds
   P by comparing the old and new paths. When a node after P carries assistant
   output, it advances `thread.reasoning_epoch` and saves the set of output
   nodes before P whose reasoning was sendable as `app_meta`
   `reasoning.keep.<thread>` (bound to the new epoch; replay sends reasoning of
   a node when its epoch is current or it is in that set). Appends, truncations
   and the active-Turn summary (no output after P) change nothing. A model
   change or a binding retry advances the epoch and so voids the set. A fork
   copies the set with the epoch. Bound: the set holds at most the output
   nodes on the selected path; it is rewritten only at an epoch advance.
   Owner: the Thread; Thread deletion removes it in the same transaction as
   the other per-Thread `app_meta` keys.
3. Selective automatic edits run only at a boundary where the latest assistant
   output on the selected path has no tool call (`history::open_tool_loop`):
   proactive spill, the overflow maximal trim and spill stages, and the
   large-input trim after transient failures. While the loop is open, an
   explicit overflow falls back to the active-Turn summary, else the ordinary
   prefix summary (attempt record `midturn.reason = "open_tool_loop"`); the
   transient-failure trim is recorded as `{"outcome":"deferred"}`. The ordinary
   prefix summary itself still runs mid-loop by the order decision above; by
   rule 2 it stops all reasoning after its edit point, including the open
   loop's thinking (judgement: documented as tolerated by Anthropic, which
   degrades a tool turn without its thinking; recheck if a provider rejects
   it). Explicit user edits (`compact --spill/--omit/--trim/--restore`,
   `--maximal`) run when requested; rule 2 keeps them safe (judgement: user
   intent, and refusing would strand an overflowing Thread).
4. No `drop_block` or other vendor knob is sent. When a provider returns the
   documented binding rejection ("Invalid `signature` in `thinking` block ...
   bound to a different conversation"), the Driver saves the failed attempt
   with `automatic_retry.reason = "thinking_binding"` and
   `thinking_binding_retry`, advances the reasoning epoch in the same
   transaction (no reasoning saved before it is sent) and resends once per
   Turn generation until a success. It does not consume the ordinary retry
   budget and records the `thinking_binding_retry` diagnostic event. A second
   rejection follows the ordinary recovery.

Not validated against a real provider (no paid calls, by decision); the basis
is Anthropic's preserved-thinking documentation and competitors shipping the
same full-replacement shape.

## Summary request usage (#133 / D-020)

Every summary request (map and reduce, from explicit `--summary`, argument-free
compact, #50 stage 2 and #125 proactive compaction; all share
`summarize_with_policy`) records the provider-reported usage. Values kept:
`input_tokens`, `cached_tokens`, `cache_creation_tokens` (#127) and
`output_tokens`, each as `{known_sum, unknown_count, overflow}`. A request is
counted durably before it is sent with all four values unknown; a response (or a
failed response carrying usage) resolves the values it reports. Values the
provider does not report, transport failures and interrupted requests stay
unknown. No estimate is made. Cached parts send nothing and add nothing.

Storage: one `app_meta` record per summarizing compaction, key
`summary.usage.<thread_id>.<uuid>`, written by the Compactor through DbWorker
(before and after each request). The record holds thread, the snapshot's current
Turn/Run (null outside a live Run), connection/model, the first-request time,
and the counters. No record exists when a compaction sent no request. Size is a
few hundred bytes and bounded by the per-compaction map/reduce request limits;
readers skip values above 16 KiB. Records are retained like
`model_attempt.usage_json` (lifetime accounting facts; soft-deleted Threads keep
them, unread). They are counted in logical history bytes through the existing
`gc_bytes_app_meta_history` partial index (schema 18 adds the prefix). No table,
worker, queue or event is added.

Per event: `CompactResult.summary_usage` carries the same counters, so queued
compaction results, `automatic_trim_attempt` summaries and the summary node's
out-of-band payload metadata (`summary_usage`, never sent to the model) expose
them. A rejected compaction that already sent requests attaches
`summary_usage` to its error details. `compact list` reads it per event from the
row it already selects (result, error details, or the summary node), with no
additional query.

`usage get` (ungrouped) adds `compaction`: `compactions`, `requests`,
`map_requests`, `reduce_requests`, the four counters and `malformed_records`.
It reads the selected Threads' records by primary-key range (one join over the
selected Thread list). Turn/Run scopes keep records whose snapshot saw that
Turn/Run as current; a period selects by first-request time. `model_attempts`
remains conversation requests only. Coverage `summary` is `recorded`. Grouped
`usage get --group-by` adds a separate `compaction` object and `total` object
beside the unchanged `model_attempts` and request-size/cache fields. Grouping uses
the record's first-request `started_at`, including the exact half-open period
boundary. Invalid/missing times remain unassigned and are counted as unknown.
The metadata scan uses the existing 10,000-candidate bound, rowid watermark and
cursor continuation independently of the model-attempt scan; group pagination
still pages the resulting groups. Malformed/oversized records are excluded and
reported, never converted to zero. Combined token sums preserve known/unknown/
overflow semantics; fields not reported for one request type remain unknown.

## Proactive selection and spill evidence (#215)

Automatic compaction attempts eligible reducing spills before requesting a
summary. An open tool loop retains its existing safety fence; a driver gate
must not suppress a newly eligible reducing spill merely because the summary
scope is escalating. Record bounded anonymous candidate, adopted and skip
counts by latest protection, unsafe or incomplete group, already spilled,
non-reducing projection, open tool loop and driver gate in the existing
`automatic_trim_attempt`; do not record content or introduce a new worker.
Adopt oldest eligible reducing spills until the calibrated need is projected
to be met or candidates are exhausted, before grouping or bundling. Then take a
fresh snapshot, subtract the actual replay-character reduction from the
calibrated removal need, and only if a need remains select the summary window.
Groups already spilled enter it as placeholders, not original tool outputs.
Bundle the selected window into existing segments (`segment_chars`, default 64,000) only after
this selection. Record whether spill, fresh measurement, selection and bundling
ran. A summary may proceed with an unspilled candidate only when its anonymous
skip reason is recorded. If spill alone reaches the target, do not summarize.

With a calibrated target, automatic prefix summary selects oldest-first whole
snapshot groups until projected replay-character removal reaches the remaining
target. Never split a group or cross the existing protection, unreflected-user,
incomplete-group and latest-complete-tool fences. If the safe prefix is too
short, select all of it and record the projected shortfall. Without a calibrated
plan keep existing selection. Manual compaction and explicit overflow retain
their existing contracts. Previously automatic summary selected the whole safe
prefix even when a smaller whole-group window could meet the target. This
projection is a planning estimate, not a semantic-retention or token guarantee;
actual provider usage still verifies the target on a subsequent request.

## Calibrated proactive target (#209)

The target is 80% of `auto_at_tokens`. In the #208 observations at the
200,000-token threshold, W7 had five post-compaction inputs in the 120,000 to
159,999 range, followed by a median 39 requests to the next marker; W3 had
four followed by a median 39.5. W7's five inputs at or above 180,000 were
followed by a median 9 requests, versus W4's 17 with median 6. These are
nearest-request observations, not proof of causal intervals or of what each
summary retained. The 160,000 target is the upper bound of the observed
120,000-159,999 band and retains more context than the 8,000-14,000 outcomes.

The #153 token-growth retry gate was replaced by #180's eligible-source witness:
current `proactive.rs` clears that witness on a successful adoption. An adopted
200K-to-190K reduction is therefore not a no-progress outcome; the next fresh
report below 200K returns normally, and 10K of subsequent growth can cross the
threshold again in a few requests. Neither gate required a useful post-adoption
target.

Calibrate the character-reduction plan from input tokens and serialized input
characters recorded for the same completed request on the same connection and
model. Use integer arithmetic and round the required reduction upward. This is a
prediction, not a tokenizer or a guaranteed bound: fixed instructions,
attachments, model changes and differing text can change the ratio. When the
pair is unavailable, preserve the existing behavior; never invent a ratio.

An adopted proactive attempt waits for fresh provider-reported usage. Record
whether it reached the target on the triggering attempt's existing observation.
Above target, an eligible next attempt must escalate its source scope, older
Turns first, then the active-Turn middle, rather than repeat the shallow plan.
Do not treat unknown or stale usage as success or failure. Respect existing
atomic adoption, cancellation, protected groups, bounded work and no-progress
fences. If no wider safe source exists, defer rather than repeatedly summarize
identical sources. A provider that never reports usage retains the existing
behavior; unmeasurable target achievement is not represented as success.

The outstanding target is one bounded Thread-owned `app_meta` value
(`compact.automatic_target.<thread>`; at most 1 MiB), not a new worker or
queue. It stores the triggering attempt, target, scope, Turn and bounded
post-adoption candidate witness; it is removed on Thread deletion, target
success or connection/model change, and replaced by the next adopted target
attempt. Fresh verification is also attached to the triggering attempt's
existing `automatic_trim_attempt` record. For an insufficient active-Turn
summary, only growth of eligible middle candidates permits another attempt in
the same Turn; incomplete candidate evidence does not establish growth. A new
Turn restores ordinary prefix selection, including after an insufficient prefix;
the outstanding shortfall remains eligible below the threshold, but must not
force active-Turn-first ordering across that boundary. Spill-only shortfalls
retain their separate escalation policy. Local replay-character changes guide
whether to widen in the same attempt; only the next provider report verifies
the outcome. No independent token or output-size guarantee is implied.

## Owner amendment: four-stage automatic escalation (#215)

In one automatic attempt, run closed-Turn spill, closed-Turn summary, active-
Turn spill, then active-Turn summary. Recompute remaining calibrated need from
a fresh replay-character measurement after each stage; skip later stages once
the target is met. Record each stage as run or skipped, with bounded anonymous
reason and actual removed characters. Group and bundle summaries only after the
corresponding spill and select oldest-first whole safe groups for remaining
need. Uncalibrated attempts retain the existing escalation fallback.

D-189 no longer prohibits selective active-Turn spill while the tool loop is
open. It remains forbidden to edit between a call and its result. A mid-Turn
spill adoption atomically advances the reasoning epoch and clears kept reasoning,
so no earlier reasoning is replayed. Keep the latest complete group, incomplete
groups and unreflected user sources protected. Anthropic documents graceful
degradation for missing tool-use thinking; live Anthropic validation remains
outstanding. Closed-Turn replacements retain the positional rule. Summary
material includes readable reasoning text from all epochs, without encrypted
or opaque data; this does not change replay eligibility.

Automatic mid-Turn summary is likewise bounded by remaining need: choose the
oldest whole safe groups before the latest complete tool group, never crossing
request, user-instruction, unreflected or incomplete fences. Its adoption
atomically advances the reasoning epoch and clears the kept set. Record the
remaining need before and after. The prior #182 all-round scope is superseded
for this automatic path; manual and overflow contracts are unchanged.

### #215 owner amendment: post-commit data safety

Each automatic stage may commit independently. A later storage error,
cancellation or target remeasurement failure must retain the committed selected
path, original nodes, replacement/source provenance and reasoning epoch without
partial adoption. After the fault is removed, a normal automatic or manual
compaction must remain possible. Missing best-effort attempt/diagnostic records
after storage failure are follow-up work, not this data-safety gate. This decision
does not adopt the proposed recovery journal or guarantee bookkeeping writes
when storage rejects them. The fault matrix and its synthetic-fault limitations
are recorded in the validation record.

## Adopted: recoverable automatic attempt record after post-commit failure (#226)

Status: **Adopted and implemented** (option A below; the leader adopted the
recommendation). The analysis sections 1-3 keep the pre-implementation line
references of source baseline `origin/main` `ca2505f7`; the implementation notes
at the end of this section list what differs from the proposal. This section does not change the
#215 data-safety contract above; it only concerns the attempt record, the
pending target, the #180 guard and the automatic-active witness.

### Current write sequence

One automatic attempt in `src/control/driver/proactive.rs` performs these
durable writes, each its own transaction on the DB writer:

| # | Write | Location |
| --- | --- | --- |
| W0 | witness `compact.automatic_active.<thread>` upsert + status delete | `proactive.rs:360-366` |
| S1 | closed-Turn spill adoption (path, replacements, provenance) | call `proactive.rs:393-396`, tx `compaction.rs:514-551` |
| S2 | closed-Turn summary adoption | calls `proactive.rs:913-930` and the uncalibrated late prefix `proactive.rs:987-1006`; tx `compaction.rs:1137-1145`, `compaction/summary.rs:13-55` |
| S3 | active-Turn spill adoption + epoch advance | call `proactive.rs:797-800`, tx `compaction.rs:540-549` |
| S4 | active-Turn summary adoption + epoch advance | calls `proactive.rs:838-845` and the uncalibrated `proactive.rs:786-791`; tx as S2 |
| W1 | value-matched witness DELETE | `proactive.rs:500-511`; its error returns at `proactive.rs:517` |
| W2 | exception record (`outcome:"exception"`, `stages`, `spill_counts`) | `proactive.rs:549-566` |
| W3 | guard save or clear (#180) | `proactive.rs:658`, `proactive.rs:683` (reads at `:626`, `:630`, `:634` can also fail) |
| W4 | final `automatic_trim_attempt` + pending target `compact.automatic_target.<thread>` | `proactive.rs:724-743` |

Re-measurements (`proactive_history_characters`, `compaction.rs:675-695`) run
after S1 (`proactive.rs:407`), after S2 (`proactive.rs:951`, `:430`) and before
and after S3/S4 (`proactive.rs:794`, `:825`, `:867`). Any `?` between a stage
commit and W4 leaves the stage committed and skips every later write.

### 1. What is lost or stale, per failure

"Stage committed" means S1-S4 returned `Ok` with a non-empty result. Every row
below applies equally after S1, S2, S3 or S4, because none of the records is
written inside a stage transaction.

| Failure after a committed stage | Attempt record (`model_attempt.error_json.$.automatic_trim_attempt` of the triggering attempt) | Pending target | #180 guard | Witness |
| --- | --- | --- | --- | --- |
| Storage error at a later stage, re-measurement or guard read; W1 succeeds | W2 runs. If W2 succeeds: `outcome:"exception"` with per-stage `outcome`/`adopted_groups`, but no summary node, source IDs, usage, `target_plan` or `trigger`. If W2 fails: the record is absent, or a previous record on the same attempt remains | Not written. A previous attempt's target, if any, remains | Pre-attempt value retained (W3 not reached) | Removed |
| Non-storage re-measurement/snapshot rejection (for example the Run is stopping); W1 succeeds | As above; W2 is normally reachable because the DB is healthy | Not written | Pre-attempt value | Removed |
| W1 (cleanup) fails | Absent: `cleanup?` at `proactive.rs:517` returns before W2 and W4 | Not written | Pre-attempt value | **Retained** |
| W3 or W4 fails (DB failing at the end) | Absent (W4 is the only complete write) | Not written (same transaction as W4) | If W3 succeeded and W4 failed: saved or cleared as for success, without a matching record | Removed (W1 before) |
| Cancellation after a committed stage, DB healthy | Complete: `committed_result` (`proactive.rs:520-547`, `:751-773`) and W4 record `adopted`, `cancelled:true`; the pending target is written | Written | Saved/cleared normally | Removed |
| Cancellation, then W1/W3/W4 fails | As the matching storage row | As that row | As that row | As that row |

Additional gaps found while reading the code:

- The uncalibrated active-Turn summary (`proactive.rs:786-791`) and the
  uncalibrated late prefix summary (`proactive.rs:987-1006`) neither update
  `stages` nor push to `committed`, and have no fault checkpoint. W2 after them
  reports those stages as `not_reached`, even though they committed. The #215
  matrix (`proactive/fault_tests.rs:81-86`) covers only the four calibrated
  call sites.
- The #215 "Storage" fault (`fault_tests.rs:377-383`) fails an unrelated TEMP
  table write, so W2 and W4 can still succeed in that test. The earlier probe
  (`/tmp/215-postcommit-failure-report.md`, test machine only) rejected the
  production W1 DELETE and observed pending target 0, adopted record 0, witness 1
  at all four boundaries. No test rejects W2, W3 or W4 at their production sites.

What each loss affects:

- **Attempt record:** diagnostics only. Readers are `compact list`
  (`readback/compact.rs:97-101`, outcome via `COALESCE(...,'recorded')`) and
  `list_observation.rs:106,165` (`automatic_recorded`). The trigger does not read
  it. Re-triggering is prevented by `next_request_pending`, which is derived from
  replacement timestamps (`context_usage.rs:129-160`), not from this record. A
  lost record cannot cause a repeated compaction of the same measurement.
- **Pending target:** continuity of the #209 shortfall only. Without it, a next
  report between the target and the threshold is `BelowThreshold`
  (`proactive.rs:135-137`), so the shortfall does not continue until the
  threshold is crossed again. #209 verification for this attempt is never
  attached. An older target that was verified `insufficient` and not removed
  (`proactive.rs:100-133` removes only `met` or a connection/model change)
  keeps steering `insufficient_prefix`/`insufficient_spill`/`exhausted_active`
  with that older attempt's scope. Spill scope has no Turn check
  (`proactive.rs:133-134`).
- **#180 guard:** no added suppression. W3 is skipped, so the pre-attempt guard
  (or no guard) persists. That guard admitted this attempt, so the stages it
  admitted are new relative to it. The committed path rewrite makes
  `same_witness`/`unchanged` false (`proactive_guard.rs:88-99`), which forces a
  fresh forecast at the next boundary. Consumed candidates are gone and
  remaining ones get the same admission decision as at the start of the failed
  attempt. Lost: rejection counters and reason (diagnostics), `refresh_revision`
  and `summary_frozen_current_turn` (`proactive_guard.rs:53-77`). Their absence
  makes the next decision more permissive, not less. This is derived from
  reading the code; no test asserts it.
- **Witness:** display only. The only reader is the activity footer
  (`frontend/activity.rs:245-273`). It shows the witness only for the same live
  Run and a `preparing`/`running` Turn. See 3.

### 2. Options for a recoverable record

**A. Stage fact in the stage's own adoption transaction (preferred).**

- In W0's existing transaction, also write a provisional record on the
  triggering attempt: `$.automatic_trim_attempt = {"outcome":"unknown",
  "provisional":true, "token":<uid>, "reason":"proactive_threshold",
  "trigger":{...}, "target_plan":{...}|null, "committed_stages":{}}`. Use the
  same `WHERE id=? AND (error_json IS NULL OR json_valid(error_json))` as
  W4. If W0 fails, no stage runs (`proactive.rs:366` returns first), so nothing
  is lost.
- Pass an optional `AutomaticStage { attempt_id, token, stage, target }` into
  the six automatic Compactor entry points (`spill_staged_cancellable`,
  `spill_reducing_cancellable`, `summarize_prefix_guarded`,
  `summarize_target_prefix_guarded`, `summarize_active_guarded`,
  `summarize_target_active_guarded`). From there it goes to `spill_snapshot`
  and to `SummaryAdoption`. Manual and overflow callers pass `None`; their
  contract is unchanged. After `select_path` and the epoch update, inside the
  same transaction (`compaction.rs:533-549`, `summary.rs:35-52`), run one
  primary-key `UPDATE model_attempt SET error_json=json_set(error_json,
  '$.automatic_trim_attempt.committed_stages.<stage>', json(?fact)) WHERE id=?
  AND json_valid(error_json) AND
  json_extract(error_json,'$.automatic_trim_attempt.token')=?token`.
- The fact is a fixed field set with no content and no ID lists: `stage`,
  post-commit `selection_revision` and `reasoning_epoch` (read in the
  transaction), `spilled_groups`, `source_nodes`, `created_parts`,
  `reused_parts`, `summary_node_id` (at most one), `source_characters`,
  `summary_characters`, `summary_limit`, `model_requests`, `summary_usage`
  counters. `removed_characters`/`remaining_after` are absent (unknown) until
  re-measurement. Ordered provenance stays authoritative in `replacement_source`.
- When an `AutomaticStage` has a calibrated target, the same transaction also
  upserts the pending target `{attempt, target_tokens, scope, turn,
  marks:null, provisional:true}`. Scope is `spill` for S1, `prefix` for S2 and
  `active_turn` for S3/S4, the same mapping W4 uses (`proactive.rs:712-723`).
  `marks:null` matches the existing coverage-limited baseline (`proactive.rs:703-711`,
  `.ok().flatten()`): later same-Turn active widening is not established
  (`proactive.rs:240-248`). That is the documented conservative behavior, not a
  new rule.
- W2 and W4 keep their content but carry the DB-side facts forward instead of
  replacing them: `json_set(json(?new),'$.committed_stages',
  json_extract(error_json,'$.automatic_trim_attempt.committed_stages'))` and
  `token`. W4's pending target overwrites the provisional one, as today.
- Bounds: one record per triggering attempt (the existing rule; a later Run on
  the same measurement still overwrites it); at most four facts, because each
  stage runs at most once per attempt (`proactive.rs:386`). Each fact has a fixed
  field set. One pending-target key per Thread (the existing key, 1 MiB check at
  `proactive.rs:716`). No new table, key namespace, worker, queue, timer or
  migration. The existing JSON field and the `automatic_compaction_turn` partial
  index (`migrations/0015_readback_indexes.sql:5`, `storage/current.sql:394`)
  already cover an object at `$.automatic_trim_attempt`.
- Idempotence and reconciliation: none is needed. The fact is written exactly
  once, atomically with the replacement it describes. Re-running a `json_set`
  with the same value is a no-op. No startup step reads or rewrites these
  records.
- If the DB keeps failing: a stage transaction that fails commits neither the
  replacement nor its fact. A stage that commits has its fact. W1-W4 may all
  fail; the record then stays `outcome:"unknown"` with the exact committed stage
  set, which is the required complete-or-unknown state. The proposal never
  depends on a later write succeeding. #209 verification
  (`proactive.rs:121`) attaches to the provisional record as well.
- Cost: one extra primary-key UPDATE (and at most one app_meta upsert) inside
  an existing write transaction per committed stage. Measured; see the
  implementation notes.
- Residual unknowns: if the triggering attempt's `error_json` is non-NULL
  invalid JSON, W0 and W4 already skip it today. The stage UPDATE then matches no row
  and adoption proceeds (diagnostics never block adoption), so no record exists.
  This limitation already exists; it is not new. A stage
  transaction that fails because of the fact UPDATE rolls the adoption back.
  That is acceptable, because the same writer failure would fail the commit.

**B. Bounded journal reconciled later (not recommended).** A Thread-owned
`compact.automatic_journal.<thread>` key holding at most four stage facts.
To survive the failures above it must also be written inside the stage
transaction, so it is option A plus a second location. It also needs: a
reconciliation write into `model_attempt` at the next boundary or in
`interrupt_previous_work` (`control/state.rs:496-503`). That write can fail
too; the journal then stays and the next try repeats it idempotently with
`json_set` plus delete in one transaction. It needs Thread-deletion cleanup
(`control/db.rs:2575-2583`), a startup `GLOB` scan bounded by the number of
Threads holding a journal, and rules for a journal whose attempt row is gone.
It adds code and tests without recording anything A cannot. A journal written
outside the stage transaction gives no improvement, because it is just another
write that can fail separately.

**C. Roll back the adoption when its record cannot be saved.** Option A gives
exactly this inside one transaction. A separate post-commit undo would violate
#215 and is rejected.

### 3. Stale automatic-active witness

- It cannot suppress compaction, indefinitely or otherwise. `maybe_compact`
  never reads it and overwrites it with an upsert (`proactive.rs:360-366`).
  Manual compaction and the guard do not read it. The only other reader is the
  activity display (`frontend/activity.rs:245-273`).
- Clearing: the owning attempt deletes it with an exact-value DELETE, so a
  retiring owner cannot remove a newer witness (`proactive.rs:499-511`).
  Startup removes every witness (`control/state.rs:500-503`; tests
  `state.rs:1018-1030`, `fault_tests.rs:219-233`). A driver storage failure
  latches `fail_storage` and begins owner shutdown (`control.rs:1159-1165`,
  `control.rs:1068-1084`), so a stale witness survives at most until the next
  startup.
- Display exposure: in the failing process, if `stop_without_driver`
  (`control.rs:1180`) also cannot save the stopped Run, the Run and Turn rows
  may remain `live`/`running`. The footer could then show automatic compaction
  until the owner exits. Not tested; unknown.
- Hygiene gap: Thread deletion removes the target, guard and status keys but
  not the witness (`control/db.rs:2575-2583`). Deletion requires stopped Runs,
  so the display filter hides it, and startup deletes it. Proposed: add the
  witness key to that existing DELETE (one parameter, same statement).
- No new detection is required. Moving W1 into W4's transaction is not
  proposed, because it would not help when the DB is failing.

### 4. Recommendation and proof

Smallest change: option A. Write the provisional `outcome:"unknown"` record in
W0's existing transaction. Write each stage's fixed-size fact, and the
provisional pending target when the attempt is calibrated, inside that stage's
adoption transaction. Have W2/W4 carry `committed_stages` forward. Optionally
add the witness to the Thread-deletion DELETE. Invariant: for every automatic
attempt whose W0 committed, the triggering attempt's record is either a final
outcome or `outcome:"unknown"`, and in both cases `committed_stages` is
exactly the set of committed S1-S4. The #180 guard and the witness need no
change, by the analysis above. Readback shows `unknown` through the existing
outcome column. The CLI/design outcome lists gain that value when adopted.

Tests, reusing the #215 harness (`src/control/driver/proactive/fault_tests.rs`,
`fault_boundary` capture and `checkpoint`):

1. For all four faults × four boundaries, after the driver joins and again after
   reopen, assert that `committed_stages` keys equal the stages whose
   replacement is selected. Assert that the faulted stage's
   `selection_revision`/`reasoning_epoch` equal `fault_boundary`, that the
   outcome is final or `unknown`, and that facts contain no arrays or text
   beyond stage/node IDs. Cancellation expects the final `adopted` record.
2. New fault "persistent": after the checkpoint, TEMP triggers reject every
   `model_attempt` UPDATE and every `app_meta` write, so W1-W4 all fail. Assert
   `unknown` with the exact stages, the provisional target (calibrated) and the
   retained witness. After reopen: witness removed, record unchanged (no
   reconciliation ran), and a fresh report verifies the provisional target,
   with `target_verification` attached to the same record.
3. Production-site rejection of W4 only (trigger `WHEN
   json_extract(NEW.error_json,'$.automatic_trim_attempt.outcome')<>'unknown'`),
   and separately of W2 and W3. This closes the caveat in the #215 validation
   record that only an unrelated write was failed.
4. Atomicity: reject the stage-fact UPDATE itself. Assert the stage is not
   adopted: path, epoch, `replacement` and `replacement_source` unchanged.
5. Uncalibrated call sites (`proactive.rs:786-791`, `:987-1006`): add
   checkpoints and run cases 1-2 there.
6. Guard: after each storage fault, assert the persisted guard is byte-equal to
   the pre-attempt guard. After reopen, a fresh above-threshold report starts an
   automatic attempt (`AutomaticCompactionStarted`). This proves that neither
   the guard nor a witness suppresses compaction.
7. Readback unit test: an `unknown` provisional record appears in `compact
   list` as `automatic_reduction` with outcome `unknown`.

Limits: synthetic SQLite trigger faults on disposable DBs, not OS disk-full or
I/O failure.

### 5. Implementation notes (as built)

Code: `src/compaction/attempt.rs` (provisional record, stage fact, finish SQL),
the six automatic Compactor entry points take an optional `AutomaticStage`
(manual and overflow callers pass `None`), and `src/control/driver/proactive.rs`.
Differences from the proposal and clarifications:

- **Provisional record (W0).** Written in the witness transaction with
  `outcome:"unknown"`, `provisional:true`, a per-attempt random `token`,
  `reason`, `trigger`, `target_plan` (null when uncalibrated) and
  `committed_stages:{}`.
- **Stage fact.** Written by the stage's own transaction after the path and
  epoch updates, through `UPDATE ... WHERE id=? AND json_valid(error_json) AND
  token matches`. Fields: `stage`, `selection_revision`, `reasoning_epoch`
  (read in the transaction), `spilled_groups`, `source_nodes`, `created_parts`,
  `reused_parts`, `summary_node_id`, `source_characters`, `summary_characters`,
  `summary_limit`, `model_requests`, `summary_usage` (the existing fixed counter
  object) and `removed_characters`. `removed_characters` is the exact
  replay-projection difference of the replaced items under the adoption
  snapshot epoch: spill uses the replaced groups' projections before and after;
  summary uses the existing source-characters measurement minus the summary
  payload projection. It excludes the effect of an epoch advance on other
  reasoning; only the driver's later re-measurement (`stages.*.removed_characters`)
  establishes whole-history removal. Measured fact size: 304 bytes for a spill
  stage on the synthetic fixture. The facts hold no arrays and no text other
  than the stage name and one summary node ID.
- **Provisional pending target.** Calibrated attempts upsert the existing key in
  the same stage transaction, with `marks:null` and `provisional:true`; scope
  `spill`/`prefix`/`active_turn` as W4 maps it. W4's target, when written,
  replaces it and has no `provisional` field.
- **W2 and W4 keep facts.** Both use one finish statement (`FINISH_SQL`) that
  replaces the record body but copies `committed_stages` and `token` from the
  stored record when the token matches. The W2 body now also carries `reason`,
  `trigger` and `target_plan`, so an exception record no longer loses them.
- **Uncalibrated sites.** The uncalibrated active-Turn summary and the late
  prefix summary now record their stage (`stages`, `committed`, stage fact) and
  have the same fault checkpoint as the calibrated calls. Previously W2
  reported those stages as `not_reached` and a cancellation record omitted them.
- **Hygiene.** Thread deletion also removes the automatic-active witness key
  (the existing statement gained one parameter).
- **Bounds and ownership.** At most four facts per record (each stage runs at
  most once per attempt), one record per triggering attempt, one pending-target
  key per Thread. No new table, key namespace, worker, queue, timer, migration
  or startup step.
- **Readback.** `compact list` shows `unknown` through the existing outcome
  column; `unknown` joins the existing outcomes (`adopted`, `rejected`,
  `exception`, ...).
- **A stage-fact failure rolls the adoption back.** If the fact UPDATE or the
  target upsert fails, the whole stage transaction fails, the replacement is not
  adopted, and the driver takes its existing storage-failure path.
  A record whose `error_json` is non-NULL invalid JSON matches no row; adoption
  proceeds without a fact, as W0 and W4 already behaved.

#### Cost measurement

Synthetic fixture only (`active_turn_fixture_with` from the compaction unit
tests, two 20,000-character tool results, a temp-dir SQLite file; macOS ARM64,
debug build, one disposable DB per run; no provider; the summary stage used a
loopback proxy). The same stage call ran with and without the automatic stage
(three runs each), and the time inside the stage fact function (the added
`SELECT`, `UPDATE` and target upsert) was captured separately. Temporary
instrumentation, not kept in the tree (a wall-clock measurement does not belong
in the test suite).

| Stage | Call without fact (us, 3 runs) | Call with fact (us, 3 runs) | Added statements inside the transaction (us, 3 runs) |
| --- | --- | --- | --- |
| active spill | 14,029 / 9,194 / 9,215 | 9,338 / 9,365 / 9,249 | 225 / 211 / 210 |
| active summary (1 request) | 13,265 / 11,242 / 11,094 | 11,363 / 11,325 / 11,953 | 233 / 234 / 249 |

The first run of each row includes cold-start effects, which is why the
no-fact call is slower there than the with-fact call. The measured added cost
per stage was 210-249 us, about 2 percent of a call that adopts one group on this
fixture. It is a synthetic debug-build measurement, not a production bound,
and is not asserted anywhere.

#### Tests

`src/control/driver/proactive/fault_tests/record_tests.rs`, reusing the #215
harness (`prepare`, `checkpoint`): for each of the four boundaries and each of
nine faults (the four #215 faults, persistent write failure, rejected real
exception write, rejected real final write, rejected real guard write, and
rejected stage fact), the record is final or `unknown`; recorded stage names
equal the expected stages; the spill-group sum and the summary node IDs of the
facts equal the replacements the Thread adopted; each fact has fixed fields,
and its revision and epoch equal the captured adoption values; the persisted
#180 guard rows equal the pre-attempt rows wherever the attempt's own guard
write is not reached or is rejected (a cancelled or final-write-rejected attempt
legitimately saves its normal guard first); the witness is retained only after
a rejected cleanup or a persistent failure; the provisional pending target has
the stage's scope. After restart the record is byte-identical to the live one
(nothing is reconciled or rewritten), `compact list` shows `unknown`, and a
later above-threshold report starts a new automatic attempt and verifies the
earlier target. A rejected stage fact leaves the epoch, replacements and path
as they were. Two direct tests drive the uncalibrated active-Turn summary and
the late prefix summary through a real history with the fault checkpoint. A
database unit test covers witness removal on Thread deletion.

## Post-compaction input (#230)

`compact list` shows, for each adopted summary (automatic or manual), the
provider-reported input tokens of the first model request of the Thread that
started after the replacement's `created_at`, as `post_compaction_input_tokens`
with that request's `post_compaction_attempt_id`. Both are null when no later
request exists; the tokens are also null when that request has no reported input
(unfinished, failed, unreported). The first request is used even if it failed:
the next request's value would describe a different context. Nothing is
estimated. The parameters in effect stay on the event (`trigger.auto_at_tokens`,
`trigger.input_tokens`).

Derivation is read-time and stores nothing: `turns_by_thread_end` walks the
Thread's Turns by `ended_at >= adoption` in order and `attempts_by_turn` finds the
first attempt after the adoption, so the first match ends the scan; an open Turn
is the fallback. One such lookup runs per retained replacement row (at most the
page candidate cap; 5 for the latest-compaction display). Failed or rejected
proactive attempts have no replacement and show null.

Conversation marker (#230): see [tui-presentation](tui-presentation.md). The
retry condition shown on a skipped or failed attempt is the existing
eligible-content gate (new or grown spill/summary candidates against the saved
evidence); the issue's 10% growth rule is not implemented (see D-230-2), so no
percentage is displayed.

Linkage limits (#230 review): the fields are carried by adopted summary events
(`kind` `summary`, `compact` or `automatic_reduction` with a summary node); spill
and range-omission events and attempts without a summary do not carry them (the
keys are absent, which is not-applicable, as distinct from null on a summary,
which means no later request yet or no reported input). Several adoptions before
one request share the same `post_compaction_attempt_id`; that input reflects all
of them, so do not sum effects. A fork inherits its parent's replacements; for those
the keys are omitted (not applicable) and the marker shows no later-input part. Only the latest 200 events are
searched for the conversation marker.

## Pruned replacements (#235)

A replacement with `state='pruned'` was released by `thread prune`: its
originals are no longer held by the pruning Thread, so `compact --restore`,
hidden-marker re-summarization and `history get --parts` sources cannot use it.
The restore path refuses at preparation and again inside the commit transaction
(`invalid_state`, "originals were pruned ... not retained"). Spill placeholders
keep their `retrieve:` hint; fetching the original reports it as pruned. The
summary node, its parts cache and summary text are not removed by the prune
itself; cache entries whose sources the GC tombstones are collected by the
existing summary-cache sweep. Compaction never reuses a pruned replacement.
