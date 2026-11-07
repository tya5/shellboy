# 検索filter・provenance・読み取り専用の履歴関係

2026-10-25、利用者が採用した三提案を実装するための採用設計。履歴／検索・CLIに対する明示依頼に基づく採用契約を本書に示す。初期仕様資料は非同梱。既存の検索・原文・採用経路と共用し、汎用グラフ編集機能や別の索引コピーを追加しない。

## 今回の範囲

検索結果の基本provenance、Thread／node kind／作成期間の検索filter、会話の直接の前後・置換の出典と置換先を参照する読み取り専用APIを提供する。Run条件・fork lineage条件・distance閾値・全祖先／子孫探索・Thread所属の全列挙／タイトル付加は今回の必須範囲としない。モデルの唯一のtoolは引き続きshell、公開CLI経由で操作する。

## 検索filter

既存 `history search --query TEXT [--limit N]` に `--thread THREAD`、繰り返せる `--kind KIND` と `--exclude-kind KIND`、`--from RFC3339`／`--to RFC3339` を追加する。未指定の検索範囲は従来どおり全体で、モデルの自Threadを暗黙に補わない。

条件間はAND、kind内はOR、exclude-kindは除外。tool結果を除く場合は `--exclude-kind tool_result` とし、別のboolと重複定義しない。kindはnodeの保存kindと一致する文字列で、history list/getで確認できる。fromは作成時刻以上、toは未満。時差付きRFC3339を絶対時刻として比較し、日付だけや時差なし、逆転／空範囲は拒否する。kind一覧は各32件以内、1値64 Unicode文字以内。

Thread条件は「そのThreadが現在採用するpathと、そこからcompleted summary/spill置換の出典を再帰的に辿ったnode」を意味する。共有nodeはどちらのforkからも対象にできるが、親Threadの未採用分岐へ広げない。圧縮によって保護された元の原文も検索できる。存在しない／削除済みThreadは空配列で隠さずThreadNotFoundを返す。

filterを満たす集合を順位付けの対象にする。全体の上位だけを取ってからfilterして、実際には存在する一致を取り逃がす実装にはしない。固定sqlite-vecの能力を実測し、索引形式を複製しない。DB workerで有限の結果を返し、候補の本文／ベクトル全件をRustへ載せない。候補chunk IDの配列とwitness検証cacheは候補数に比例する。sqlite-vec 0.1.9のrowid INによって適格集合へ絞ってからL2 KNNを実行する。vec0は1回のscanが全chunkを読むため(#240)、scanは`min(適格数, 4096)`件の1回だけにする。node単位にまとめるため、従来のk=limit*8から倍々に拡大する段階(最大4096)をその順序付き行の先頭から再現し、各nodeのspanは`limit`個のnodeが揃った段階までの行だけから作る(結果は従来のscanし直しと同一。ただし距離が完全に等しい行の順序は従来と同じく保証しない)。hit組み立て(provenance/thread_scope)は最終的に返すnodeだけで行う。それでも同一nodeのchunkで埋まる場合だけ、適格集合内の正確なL2走査へ切り替える。候補metadata走査はO(chunks)、fallbackはO(chunks×dimension)になり得る。

provider embeddingはmodel名によらず、HTTP clientで単位L2長へ正規化してから索引・検索・exact fallbackへ渡す(#250)。単位長同士ならL2とcosineの順位は一致するので、次元削減した未正規化ベクトルでも順位がずれない。長さ0や非有限のベクトルは補正せず`protocol`失敗にする。既に単位長のspaceは再構築不要で、#250以前に次元削減で作ったspaceは再構築するまで未正規化のまま残る。

distanceは利用中の索引spaceのL2距離で、小さいほど近い。確率や正答率ではなく、異なるembedding model／space間では比較しない。閾値指定は今回導入しない。filterはquery embeddingを生成する前にすべて適用するという意味ではなく、DB内のベクトル順位付け前に適用する。

## Int8 conversion (#253)

`index shrink` converts the active float32 space to a new int8 space without calling an embedding API, so a search reads a quarter of the vector bytes. Explicit, with `--dry-run` as the preview; never automatic. The surface is an operation on size, not a tuning knob: no scale, percentile or precision setting exists, and no model name is consulted. `rebuild --dimensions` is unchanged and independent.

**Scale.** One global multiplier per space, `int8_scale = 127 / max|element|` over all the source vectors at conversion. Stored value = `round(element × int8_scale)` clipped to ±127 (symmetric). The scale is saved in `index_space.int8_scale` when the int8 space is created and never changes. A later element beyond the range saturates: a new embedding or a query is never refused or rescaled for it. See the measurements below for why the simple global maximum was kept.

**Lifecycle.** The existing candidate/active lifecycle, with no new worker, queue or table. The existing index workers run one bounded unit per round on the background lane, alongside and not ahead of paid work. Phases:

1. `scale`: pages of vectors, tracking the largest absolute element. Ends by creating the `candidate` space with the scale, and copying its request mode and query fingerprint from the source.
2. `copy`: pages in `vec_row_id` order. Each vector is quantized and inserted with the same row ID, with a new job and chunk row and the preview, keeping the source witness, in the unit's transaction. Search on the old space is untouched. Capacity is checked after each unit's copy (see the logical cap below).
3. Switch, when no chunk is ahead of the cursor and no job of the source is `running` (a claim in flight could still add a vector, and its billed result should not bounce off an inactive space): source `inactive`, candidate takes the source's `active` or `paused` state, active pointer moves, all in one transaction.
4. `sweep`: pages moving the source's `pending`, `failed` and `split` jobs to the new space; their saved inputs stay valid and no API is called by the move. Inactive spaces are never claimed, so they would otherwise be stranded. Then the record ends. The old space stays, inactive, with all its vectors.

**Completeness without a count.** New source vectors take row IDs above the cursor: `adopt_vector_tx` allocates `max(MAX+1, cursor+1)`, because plain `MAX+1` can reuse a deleted row ID behind the cursor. Source deletion already removes vectors in every space, the candidate included. So "no chunk ahead of the cursor" proves the candidate holds every eligible chunk, with an indexed seek rather than a scan. A chunk whose job is not `done` is skipped: search ignores it.

**New state.** `index.shrink.<source space>` in app_meta. Need: resumable progress and the failure record. Bound: one small JSON row; starting refuses while one exists. Owner: the index workers, which read it by key-range seek each round. End of life: removed on completion, on `--cancel`, or when the source stops being active before the switch (`select_space` or a finished rebuild: the later explicit choice wins, the candidate is discarded). It also blocks `index remove` of its spaces. `index.converted_from.<int8 space>`: need: finding the revert target; bound: one small row per converted space; owner and end of life: that space. `index_space.state='discarded'`: need: a free int8 candidate that may be collected; a state value, no schema change. `index.partial.<space>`: need: a replaced rebuild candidate is kept for its paid vectors but must not be selected; bound: one small row per such space; owner and end of life: that space. `index_space.int8_scale` (schema 33): need: the value must stay with the space; bound: one REAL per space; owner and end of life: the space. A column rather than app_meta because the logical-byte triggers need the element size in SQL.

**Failure and interruption.** Every unit commits its work and cursor together, so an interruption loses at most one unit and resumes by itself on the next owner start. A failed unit rolls back, saves `error` in the record and stops without pausing indexing; `index status` shows it. `index shrink` clears it and resumes at the cursor; `--cancel` retires the candidate. Only when the failure cannot even be saved does it reach the existing index worker pause. A rebuild runs independently of a conversion, in either order: a float32 candidate that activates first ends the conversion, failed or not, and discards its candidate; one that activates later replaces the int8 space, as any later choice does. Rebuild always creates float32.

**The logical cap.** A maintenance operation must not stop conversations. At the cap new work is held, and the index worker saves a pause that discards queued jobs, so a conversion must not be what gets there. Because the old space now stays, the added bytes stay as well, so the room limit is the existing `data_warning_bytes` (or the cap, if lower), not the cap. The dry run compares `logical_bytes + vector bytes` with it and start refuses as `low_room`. Each copy unit also checks after its copy, so the check sees the unit's uncommitted bytes and the preview and row overhead: crossing rolls the unit back and saves only the conversion's failure, with no pause and no discarded job. A partial candidate is discarded at once, so it does not hold room, and a later run starts over. The volume is bounded the same way (#261): the dry run reports `low_disk` (and `filesystem_free_bytes`) when the free space minus the added bytes would be below `limits.disk_free_min_bytes`, start and resume refuse it, and each copy unit checks the free space after its copy, so a disk that fills mid-run rolls the unit back, gives the partial candidate back, saves only the conversion's failure (no pause, no discarded job) and waits for an explicit `index shrink`. Unknown free space never refuses. Ordinary growth can still reach the cap later, retained spaces included. Then deleting one is an option for the user, never a model's or the system's own move.

**Search.** vec0 reads a bare JSON query as float32 and refuses it on an int8 column, so the query, insert and exact-fallback SQL wrap the parameter in `vec_int8(...)` for an int8 space and are byte-identical for float32. The rowid-IN KNN of #240 and `vec_distance_L2` work unchanged on int8 in sqlite-vec 0.1.9 (test `pinned_sqlite_vec_supports_int8_knn_rowid_filter_exact_and_delete`). The pinned crate's build defines neither `SQLITE_VEC_ENABLE_NEON` nor AVX, so distance uses the scalar float accumulation on every platform. A query embedding is quantized with the space's scale in both `history search` and history-backed `rag search`.

**`distance`.** L2 between the stored values of the space it came from, as reported by vec0: for int8, in integer steps, so around hundreds where float32 gives around 1. It is not rescaled: a division by the scale would present an estimate as a measured value and would still not be comparable across spaces. The contract is unchanged: it orders results within one space; no thresholds; no comparison across spaces, including before and after a conversion. RAG fusion uses ranks, not distances, so history hits from an int8 space merge unchanged.

**Accounting.** `vector_payload_bytes`, the logical index counter and storage observations use `dimension × 1` for int8. While both spaces exist the counter holds both.

**Retention and deletion are separate.** `index shrink` only converts. The old space stays inactive and counts toward logical bytes; status shows `retained_vector_payload_bytes` and a `retained_remove_with` per space. A person removes it with `index remove --space ID`, whose dry run lists exactly what goes ([gc](gc.md)). The same holds for the old space after a rebuild.

**Revert.** `index shrink --revert` uses the same record and atomic switch in the other direction: phase `revert` (waits for no running job on the int8 space, then switches), `requeue`, `sweep`. `index.converted_from.<int8 space>` names the float32 space, and is removed with that space; if the float32 space is gone there is no revert. `requeue` walks the int8 chunks in bounded pages and adds an ordinary `index.auto.<profile>.<node>` admission for each source whose exact segment is absent from the float32 space, and existing jobs dedupe segments that are already there. `requeue` is entered only when automatic indexing is on, not paused and under the same configuration: otherwise pending admissions would block a later rebuild from activating and spend when the switch is turned on, so nothing is queued and the preview points to `index preview` and `index backfill`. The int8 space stays too, because vectors added to it were paid for.

**Measurements.** Facts from 2026-11-07 on macOS ARM64, release profile, one temporary database holding a copy of 44,038 real 3,072-dimension unit vectors (32,779 distinct); no API call, no conversion of a real database. Query vectors are existing chunks, a proxy: real queries were not measured.

Rank agreement with the float32 top-k, exact duplicates removed (they make top-k ties look like misses), self excluded, 300 queries (mean / worst / 5th percentile):

| Scale | Top 10 | Top 50 | Elements saturated |
|---|---|---|---|
| Global max (127/0.2234), adopted | 98.77 / 80 / 90 | 98.92 / 94 / 96 | 0 |
| Max of 1,024 evenly spaced vectors | 98.80 / 90 / 90 | 98.92 / 94 / 96 | 2e-8 |
| Max of 256 evenly spaced vectors | 98.60 / 90 / 90 | 98.85 / 96 / 96 | 3.5e-7 |
| Value at about the 99.99th percentile | 99.07 / 90 / 90 | 99.14 / 96 / 98 | 9e-4 |
| Value at about the 99th percentile | 98.80 / 90 / 90 | 98.83 / 92 / 96 | 9e-3 |
| Per-dimension max | 96.23 / 70 / 90 | 96.30 / 86 / 92 | 0 |
| sqlite-vec `vec_quantize_int8` unit range | 94.90 / 70 / 80 | 95.54 / 84 / 90 | 0 |
| Truncate to 768 dimensions and normalize (same bytes) | 92.67 / 60 / 80 | 92.16 / 78 / 84 | n/a |

The global scale is right: per-dimension scaling distorts L2 and is worse; percentile variants differ by about the noise of 300 queries (roughly 0.2 points of standard error) and would add a histogram and saturation of existing data for no measured gain. Unit-range quantization does lose resolution as expected, though less than feared. Future vectors: with the scale taken from the oldest half, 0 of 16,390 newer vectors had any saturated element. Queries were not measured for saturation.

| | float32 | int8 |
|---|---|---|
| Bytes one KNN scan reads (vector chunks) | 553,648,128 | 138,412,032 (exactly 1/4) |
| KNN scan, warm cache, k=4096 with rowid IN, 5 queries | 292-396 ms | 189-192 ms |
| Whole ranking in this fixture, warm cache | 695-756 ms | 623-659 ms |

The fixture has one node per chunk (44,038 nodes), so its non-KNN share is larger than a real index with fewer nodes. Cold-cache time, the main case for reading fewer bytes, was not measured (no way to drop the OS cache here without elevated rights).

Conversion of the 44,038 vectors: 2,757 units, 109 s of unit time. Scale units: median 30 ms, max 78 ms. Copy units (32 vectors): median 49 ms, p99 95 ms, max 128 ms. Most VM steps in one unit: 14,336 of the 131,072 budget. A foreground write every 50 ms during the conversion waited: median 19 ms, p99 54 ms, max 75 ms. These are facts, not bounds. The database grew by 44,192 pages (181 MB) while both spaces existed, more than the 138 MB of vectors because of the job, chunk and preview rows.

Deleting a retained space took 137 s before this change and 0.85 s after it: deleting each job checked the `embedding_chunk.job_id` foreign key by scanning every chunk of the remaining space, which is quadratic when two large spaces coexist. Migration 33 adds the index `chunks_job`. This also speeds up collection after any rebuild. `index remove` runs as one foreground transaction, so other writes wait for it.

## provenanceと区間

検索結果の従来のnode_id/excerpt/distance/spansを維持し、基本provenanceとして保存kind・作成時刻・source_revisionを返す。原文getでも同じprovenanceを取得できる。Turn／Run／生成元Threadを確定できる場合はIDを返し、不明なものへ仮IDや最新Runを付けない。生成元の任意IDは記録されたTurnとのjoinで確定できる範囲に限る。共有nodeの生成元と採用先を混同しない。Thread指定時は `thread_scope: {thread_id, on_current_path}` を各hitに返し、現在pathに採用されたnodeと、圧縮元として対象になったnodeを区別する。Thread未指定時はこのfieldを省略し、全体検索から採用先を推定しない。nodeに存在しない更新時刻は捏造しない。

削除済み／payload回収済みnodeとstale索引は検索対象から除く。Threadの論理削除だけで共有nodeまで削除扱いにはしないため、別Threadやliveなsource関係で保持されるnodeは全体検索で見つかり得る。削除Threadだけが所有し、現在の保持rootへ到達しないnodeはGC前でも検索から除く（下記G7）。生成元Threadが論理削除されても保存済みのorigin IDは改変せず、現在そのThreadへ操作可能であるという保証を付けない。指定Thread自体が削除済みならThreadNotFound。GC後は検索に出なくても、metadataが残るnodeはget／relationsでprovenanceとtombstoneを確認できる。

spansは意味検索で選ばれた索引チャンクの原文範囲であり、キーワードの完全一致箇所ではない。body_kindごとの抽出テキストに対するUnicode scalarの開始・排他的終端、索引元のsource_revision・content_hash（そのchunk抽出テキストUTF-8のSHA-256）を返す。元JSON全体やtool raw bytesに対するbyte offsetとは扱わない。本文／rawの世代・encodingが変わった旧索引を現在の原文と偽って返さない。

同一nodeの複数chunkは一つのhitにまとめる。内容が同じでも異なるnode IDのtool/eventは別の履歴事実として返し、本文hashだけで統合しない。nodeの距離は取得chunkの最小値。spansは取得できたchunk範囲であり、そのnodeの全chunkを網羅しない。

検索結果はDB worker内で検証・順位付け・metadata読取りした時点の結果で、将来の原文保持を予約しない。検索のページcursorは導入せず、再検索は新しい状態を観測する。operation receiptによる同一操作IDの再送は元の応答を返し得るため、最新状態の確認には新しい操作IDを使う。

## 読み取り専用の直接関係

`history relations --node NODE --relation previous|next|sources|replacements [--limit N] [--cursor C]` を追加する。既定50・最大200件。previous/nextは会話edgeの入辺／出辺、sourcesはcompleted置換の出力から出典へ、replacementsは出典から置換出力へ向かう。関係の種類を混ぜず、全祖先・全子孫へ無制限に展開しない。現在の採用経路は既存 `history list --thread` が正本であり、DAGの逆探索から推定しない。

結果はitems/next_cursor。各itemは関連node IDと基本provenanceを持ち、置換関係ならreplacement ID・記録当時のsource revision/hashも返す。現在のpayload revisionと置換作成時のwitnessは区別する。同じsourceが複数の置換に使われれば別関係として取得できる。tombstoneも存在する履歴としてmetadataを返し、本文回収を架空の本文で隠さない。

cursorは対象node・relation種別・payload revision・初回watermarkへ結び付け、後から追加された関係を混ぜない。対象／種別の変更は拒否し、件数上限を超える関係を黙って切らない。関係読取りによりnode・path・Run・GC rootを変更しない。通常のoperation receiptによる読取り再送契約は維持する。

### 採用経路・summary・spillの区別

`previous`／`next` は保存された会話edgeだけを返す。forkや経路置換で共有されたnodeには複数の前後関係が残り得るため、返ったedgeをすべて連結して「現在の会話」と解釈しない。対象Threadの現在の採用順は `history list --thread` で取得する。`relations` のcursorはThreadの選択revisionに結び付けず、別Threadのforkや採用経路変更だけでは失効しない。一方、既存の `history list` は対象Threadの選択revisionが変わると旧cursorを拒否する。

`sources`／`replacements` は `replacement.state='completed'` の保存済み置換だけを返す。summaryとspillは置換出力nodeのprovenance.kindで区別し（sourcesでは指定元nodeをgetする）、summaryを会話edge上の祖先、spillを単なる次メッセージとして扱わない。出典が採用経路から外れていても置換の出典参照は残る。元の会話の前後、置換の出典、現在の採用経路はそれぞれ別の情報であり、いずれか一つから他を推定しない。同じnodeを複数の置換が参照する場合や同じ置換内に複数の出典記録がある場合は、記録ごとに取得する。

### ページング・GC・tombstoneの境界

前後関係はedgeの保存順、置換関係はreplacementの保存順とsource ordinalの順で読む。cursorは対象node ID、関係の種類、対象nodeのpayload revision、最後の位置、初回取得時の関係rowid上限を保持する。上限はedgeならedge、置換関係ならreplacement_sourceのrowidであり、次ページに初回取得後の追加を混ぜない。置換はcompletedと出典を同一transactionで保存し、完成前のレコードを後からcompletedへ更新する製品経路は設けない。

対象／関係の変更、不正な位置、対象nodeの本文回収などによるpayload revision変更、現在の関係上限より大きいwatermarkは `CursorExpired` とする。上限未満の既存関係はそのまま継続でき、無関係なnodeやedgeの追加では失効させない。対象nodeそのものが存在しない場合は既存の原文取得と同じ `TargetUnavailable` であり、空の関係一覧へ置き換えない。新しく追加された関係も必要ならcursorなしで取得し直す。

時間だけによるTTLは設けない。対象nodeのrevision変更やscope／位置の不整合で失効する。

このwatermarkは関係の追加を制限するもので、ページ間のDB全体のsnapshotや原文保持を保証しない。関係先nodeのprovenanceは各ページ取得時の現在値で、関係先だけがtombstoneになっても対象nodeのcursorは失効しない。既存GCはnodeのmetadata、会話edge、置換出典を残し、本文を回収してpayload revisionとdeleted_reasonを更新する。置換itemのsource_revision／source_hashは作成当時の記録を保持し、provenance.source_revisionは現在値を返す。原文を読むときは `history get` のtombstone_reasonも確認する。本文が空であることだけを根拠に元から空の会話と判定しない。

関係参照はGC rootや長期pinを追加しない。ページを読んでいる間も通常の保持・GC規則が適用される。cursorがあるため本文を後で必ず読める、すべてのページのmetadataが同一時点である、といった保証はしない。

## 検証の基準

filterに合わない近傍を400件以上置いても、filter対象内の候補を取得できること。Thread共有・分岐後の除外・要約／spillの出典、時差・期間境界、kindの包含／除外、無指定の既存互換、stale索引と原文範囲を確認する。関係参照はダイヤモンド・複数置換・200件超のページング・初回後の追加・cursor取り違え・tombstone・読取り副作用なしを確認する。CLI／F8／Controlの型付き経路も接続する。

利用者の既定データや実会話は試験に使わない。通常の索引試験は実sqlite-vecと制御可能なembedding HTTP fixtureを使い、実API試験が必要な場合は利用者が明示指定した接続先を使う。受入結果はvalidationへ分離する。


## Thread論理削除と検索可視性（G7）

Thread削除transaction後、意味検索とRAG historyのkeyword/semantic検索で削除Thread専有nodeを
候補から除く。索引や原文を削除せず、embedding/backfillを暗黙実行しない。同ID receipt再送は
過去の結果を返すため最新検索は新operation IDを使う。原文get/既存referenceの保持契約は変えず、
物理GC後はtombstone/revisionの既存規則に従う。削除が認証上の秘密情報消去になるとは称さない。

GCと同じ現在root（live Thread path、live Turn、receipt、未反映添付、runtime node pin、
live Threadが保持するreplacement出力）と置換sourceの逆到達性を使う。live fork共有、未採用の
live Turn node、live summary/spillから推移的に参照される元source、accepted/held receipt等は
隠さない。GCの一時markや進捗を検索可視性の正本にせず、読取時点の永続root/一時pinを照合する。

rootへ届かないことだけでは不十分で、node自身又は置換consumerが削除Threadのpath/Turn/
receipt/添付delivery/replacementに属することを保存関係から確定できる場合だけ非表示にする。
所有関係が未記録の独立sourceへ生成元Threadを推測せず、現行の全体検索を維持する。
thread_pathだけを見て未採用履歴や圧縮元を排除しない。source→consumerは既存reverse indexを使い、
循環はUNIONで有限化する。意味検索は同一nodeの判定を一回の検索内で再利用し、KNN前に除外する。
判定は2段で行う(#240): 先にconsumer閉包のroot存在だけを調べ、真ならそこで可視とする(`A OR B`のAが真)。
rootが偽のときだけ従来の全規則(root OR 削除所有者なし AND 非prune元)を評価する。1文にまとめると閉包CTEが
2箇所から参照されて毎回全閉包がMATERIALIZEされ、短絡しない。
既知の潜在コスト(実測、未対策): rootが偽のnodeでは`DELETED_OWNER`側が閉包全体に対して評価され、
`thread_replacement`に自動indexを作る計画になり約25 ms/nodeだった(単独測定、6,723 node=172 s)。
利用者DBでは全nodeがrootで可視のため発火しない。削除済みThread由来・所有者なしnodeが多いDBで
検索が遅くなり得る。フォローアップは#241で扱う。
新table/cache worker/公開filter/課金は追加しない。

Thread論理削除時は最後のthread_path/thread_replacementを所有証拠として保持する。
従来の即時消去はturnを持たないsummary/spill/旧nodeの所有情報を失わせていた。
これらはdeleted_atによりGC rootや公開の選択対象から外れ、Thread指定の新索引選択もlive joinで
除外する。GCが当該nodeをtombstone化する同transactionで削除Thread側の当該行も回収するため、
新表/markerや恒久auditを追加しない。生存共有/source/pinで本文が保持される間は所有証拠も残り、
既存logical usageへ算入する。旧版が既に消したpath所有は復元/推測せず、残るTurn/receipt等で
削除所有を確認できない原文は既存GCに従う。

## 履歴一覧の抜粋制限と保存本文の大きさ（Issue #23）

`history list --excerpt-chars N` はUnicode scalarで0〜4096、既定512。0ではexcerptは空文字とし、SQLのCASEでpayload_jsonを取得せず解析もしない。本文の存在や長さは別に `payload_chars` で返す。SQLite `length(payload_json)` による保存JSON全体の文字数であり、抽出本文長やモデルのtoken数ではない。tombstoneの回収済み本文はnull（不明）を返し、0と捏造しない。省略した一覧から必要なnodeだけhistory getで取得する。

既存のPage/HistorySnippetとcontext/selection/provenance/実行リンクは保持し、上記fieldを加える。生成元Threadはfork先と異なるためcontext.thread_idをページの選択Threadへ置き換えない。今回は重複値のページ上位への移動を見送り、汎用field選択や別modeの枠組みを追加しない。既定値のcursor identityは維持し、512以外は抜粋上限もcursorへ束縛する。50件が常に16KiBに収まる保証はせず、固定fixtureで改善量を実測して記録する。

## Shared keyword Thread scope (Issue #34)

Standalone `rag search --scope history:THREAD` reuses the semantic search scope: current Thread path and recursive sources of completed replacements (summary, spill, and omit). The SQL scope definition and current-path projection live in `history::search_scope` and are shared by semantic ranking, semantic result revalidation, and RAG keyword retrieval. A source hit may have `thread_scope.on_current_path=false`; this is membership through replacement provenance, not current adoption. Keyword retrieval does not require embedding or alter the history/index.

### Thread-scope query cost

The shared definition has two equivalent query directions: semantic ranking
expands current-path nodes toward completed replacement sources once, while
keyword candidate validation follows a source backward toward replacement outputs
and tests the indexed Thread path. Reverse traversal uses the existing
replacement_source_reverse and Thread-path indexes and cached prepared statements.
Each keyword batch contains at most 32 node IDs; it does not reconstruct the full
recursive scope. Per-node snapshot and current-path/provenance distinctions remain
unchanged. Synthetic thousands-of-node timing is recorded separately from any SLA.

## Kinds that semantic coverage omits (#254)

Semantic search ranks only indexed segments, and automatic indexing takes in the node kinds listed in `indexing.auto_kinds` (default user, summary and assistant_response). Semantic results therefore never imply that every node kind is searchable: `history search` `scope_status` and history-scoped `rag search` carry `auto_kinds` and `kinds_not_auto_indexed` (and, for `rag search`, a `history_kinds_not_indexed` warning), with a message that keyword search covers every kind. Keyword retrieval is unchanged and independent of the setting: it visits nodes newest-first in 32-node batches within the existing 2,000-node call limit and reads originals, not vectors. A kind filter is applied to the eligible set before ranking like the existing `--kind` filters; it never post-filters a global shortlist. Measured keyword speed at production data size is recorded in the validation record.
