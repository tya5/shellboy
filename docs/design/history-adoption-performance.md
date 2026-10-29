# 選択経路の末尾採用と問い合わせ index（#25 / #26）

採用設計。既存の immutable node / edge / thread_path / revision と専用 DB worker を使い、新しい表・event・worker・永続一次情報は追加しない。

## 受理済み node の末尾採用

`history::append_existing_nodes` は同一 transaction 内で生存 Thread と期待 selection_revision、cursor と実末尾の一致を検査する。caller 順序の既存 node のうち既に選択済みのものは indexed lookup で省き、未選択 node の edge と ordinal だけを追加する。追加があれば cursor、selection_revision、Thread revision、GC root_revision を各一回更新する。追加ゼロでは revision は変わらない。caller は失敗時 transaction 全体を rollback する。

Driver の受理済み receipt 採用は全 thread_path の読取り・contains・count・削除再挿入を行わず、この関数を使う。任意経路の置換は `select_path` に残す。両者の edge 追加は既存 edge を SQL の `NOT EXISTS` で INSERT 前に除く。`INSERT OR IGNORE` の重複にも起動する BEFORE trigger の再帰探索を避け、新しい edge の循環拒否は既存 trigger のまま維持する。既存 schema/trigger の変更は不要。

## Schema v10

`0010_hot_query_indexes` は導出 index 三本だけを追加する。

- `shell_active_owner(owner_run_id,state)`、active state の partial index。global wait ownership は active 部分だけを scan、Run 別 count は seek する。
- `run_thread_created(thread_id,created_at DESC,id DESC)`。ThreadList の最新 Run 相関 lookup と順序を満たす。
- `shell_terminal_time(COALESCE(ended_at,started_at))`、terminal state の partial index。raw GC の次ページの時刻 seek と rowid 順序を満たす（SQLite index の暗黙 rowid suffix を使用）。

書込み時は Run 一行につき一 index、shell 行は該当 active/terminal index の保守を負う。state 遷移時は partial index の出入りが発生する。既存行は upgrade 時に一度 index を構築し、ledger/user_version と同じ transaction で commit する。失敗は index と版を rollback する。データ・保持期限・資格情報は変更しない。

`run` の `state='live'` だけの Thread lookup は同義の `state IN ('live','stopping') AND state='live'` とし、既存 `one_live_run` partial index を利用する。global active Run/Turn は既存 index を使用済みなので追加しない。app_meta authorization は json_valid 条件を加えると不正な永続 JSON の扱いまで変わるため見送る。全 scan を無条件に消す方針ではない。

独立 RAG の `embeddings(space_id,vec_rowid)` と `chunks(hash)` は RAG schema v3 側で扱う。会話 DB migration に混ぜない。

## 測定の境界

`tests/performance.rs::performance_history_receipt_adoption` は既存 1000 / 3000 node の線形 DAG と未選択 user node 一個を使う。各試行後 rollback して同一 fixture を五回使用し、prefix、cursor、revision、root、edge 数を検査する。測定は DB worker 内の採用部分だけで、receipt query、replay、HTTP、commit/fsync、TUI は含めない。

変更前 macOS / bundled SQLite 3.53.2 の実測は `.local/baseline25.log`。P1000 p50 956226.041µs / p95 995540.667µs、P3000 p50 8598209.625µs / p95 8982066.958µs。変更後の実測は root の検証完了後に acceptance へ記録する。

#26 の SQL-only 合成 fixture は 1000 Thread、10000 Run、50000 shell、100 active Run/shell。Python SQLite 3.53.0 で同じ DB に候補 index を適用する前後を比較した。完全な Control wait 観測や ThreadSummary 構築、queue 待ち、Rust decoding、fsync の計測ではない。再現は `python3 scripts/measure-query-indexes.py`（stdout JSON）。今回の exact SQL / EXPLAIN / 各試行結果は `.local/issue26-query-baseline.json`。通常九試行、重い RAG orphan の変更前だけ一試行。

| 対象 | 変更前 p50 µs | 変更後 p50 µs | planner の変化 |
|---|---:|---:|---|
| wait ownership の三 SELECT 合計 | 4347.62 | 151.84 | shell 全件 → active index 部分 scan。Run/Turn は既存 index |
| owner active shell count | 2828.75 | 8.50 | 全件 scan → covering seek |
| ThreadList 51候補の実 SQL | 34040.25 | 165.08 | 相関 Run scan/sort → Thread/order seek |
| raw GC 128候補の実 SQL | 32090.71 | 1191.17 | 全件 scan/sort → terminal time seek |
| Thread live lookup（既存 partial に適合） | 364.54 | 6.33 | run 全件 scan → one_live_run seek。新 index 不要 |
| RAG MAX(vec_rowid) | 5587.92 | 6.67 | space hash index 範囲 → space/vec covering seek |
| RAG orphan 12500行 | 19219565.50 | 5656.00 | 相関 chunks 全件 scan → hash covering seek |

app_meta の現 SQL は scan を維持する。比較用 json_valid 条件付き SQL は既存 index を使えたが未採用。実機 Windows/Linux や利用者データは測定していない。これらの SQL 時間を製品全体の応答時間と同一視しない。


## Tool-result replay order after interrupted ownership (#37)

Cache frames retain source-path order and source stamps. Each decoded frame also
retains its optional existing group_id (capacity included in retained/live byte
accounting). A request lease may move Arc result frames next to the matching
selected response without copying their JSON or changing the cache/DAG. Original
source IDs continue through attachment binding and API-item manifest positions.
The one-pass grouped ordering/validation uses only the current lease, no extra
query per node, persistent cache, table or worker. Missing, duplicate or orphan
pairs fail before acquiring lease pins/starting HTTP. Saved results remain
immutable even if an earlier audit node was their original parent.

A current spill contains one complete tool group plus interleaved context in a
single node. Normalize its converted Values once before caching, moving its
outputs after its last call without cloning JSON or rewriting stored spill
payload/source links. Preserve the relative order of all non-result items.
