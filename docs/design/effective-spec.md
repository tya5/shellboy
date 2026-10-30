# 適用仕様

Shellboyの開発で使う採用仕様の入口です。仕様の採用と実装・検証の完了を区別します。現在地は [status](../development/status.md)、改名後の確認範囲は [acceptance](../validation/acceptance.md) に記録します。

## 読む順序

1. 本書で対象範囲と境界を確認する。
2. [Control](control.md) と [状態境界](control-boundary-map.md)、対象領域の [採用設計](README.md) を読む。
3. 操作は [利用ガイド](../guides/README.md)、公開wireは [protocol](protocol.md)、CLIは [cli](cli.md)、設定は [config](config.md)、保存は [storage](storage.md) と配布migrationを照合する。
4. 公開型・設定schema・migration・fixtureと設計の不整合を解消してから実装する。

このリポジトリ内の採用設計を開発基準とし、非公開資料や過去レビュー台帳を前提にしない。同じ事項の変更は対象設計と本書を整合させる。文書の日付・レビュー提案・現状コードだけで契約を変更せず、理由と影響を [判断記録](../development/decisions.md) に残す。

Shellboyへの改名では旧名の実行名・環境変数・既定保存先への互換を提供しない。旧データの移動は不要であり、既存環境を変更しない。新しい既定保存先は [setup](../development/setup.md) に記載する。

## Current conversation storage

Conversation schema14 records selected-path admission (13), Turn interruption
and draft indexes (14); standalone RAG remains schema3. The existing migration2
`tool_result_node(node_id)` index supports #41's retained-source query batching;
no new index/migration is required. See [storage](storage.md). Historical version
notes below describe their own releases, not the current schema number.

## 0.2.0 compatibility boundary

Issue #33 adopts current-only formats: protocol version 2; required Message/Run/Turn/HistoryBody context, HistoryBody provenance and HistorySnippet provenance/context/selection; genuinely optional Thread/fork context; current index profile/query-marker validation; no expired status-operation alias, old-owner field defaults, legacy index fallback, attachment-origin repair, or duplicated RAG degradation warnings in `scope_errors`. Conversation schema advances to v11; standalone RAG schema remains v3. Isolated startup migration removes obsolete non-object index failure counters and the unused receipt-turn-source index; fresh conversation DBs use the complete current schema. Keep the general schema migration runner, ledger/user_version checks, and protocol-version rejection. Historical compatibility notes in adopted designs are superseded by their current-only sections.

Release 0.2.0 is published without a backward-compatibility promise for earlier releases. Historical schema upgrades remain isolated in startup migration modules; Core accepts only current formats. In particular, RAG does not preserve Windows
backslash document keys: a complete synchronization indexes current slash keys
with new identities and removes absent legacy keys from active search. Recreating
the database is unnecessary; slash normalization already shipped in 0.1.6.
Do not add fallback lookup or identity migration for those keys. This records the
Issue #20 clarification; it does not trigger deletion or conversion of user data.

## Revisit and application-exit visibility (#36, #38, #39)

Adopt [Revisit](revisit.md) as one bounded self-instruction at normal Run
completion, using existing event/delivery and explicit stop/shutdown precedence.
Protocol remains v2 with additive operations and optional factual summaries;
conversation schema v12 extends derived metadata byte accounting. Existing
shutdown and [Thread browser](thread-browser.md) projections expose interrupted
latest Runs without adding a lifecycle state or automatically resuming work.

## Interrupted tool groups (#37)

Adopt [tool-group recovery](tool-group-recovery.md): startup records original
interruption facts, repairs missing current-format results atomically (including
already-terminal Turns), and cancels unapplied shell-origin deliveries before
completing groups. It never repeats OS side effects. Saved real results and DAG
edges remain immutable; only the model replay projection places outputs next
to their calls and validates complete pairs before HTTP. No schema/protocol
version, worker, queue or dependency is added.

## 変えない責務

Rust / Ratatui / Crossterm / Tokio / bundled SQLite / sqlite-vec を使い、依存版は固定する。
UIのメインOSスレッドは描画・入力・メモリ上の状態更新を担当する。I/O、DB、join、キュー空き待ちは置かない。
共有async runtime、専用DB worker、有限blocking file workerを使う。
モデルのツールはshellだけで、履歴・送信・子・待機・添付の操作は同じ `shellboy` CLIへ集める。

Controlが受付・順序・所有を管理し、Storeが短いtransactionで保存し、Driverが要求を進める。
Frontendは保存済み情報から人向け表示を作る。別のジョブエンジン、常時監視poller、汎用遷移DSLを足さない。
Coreの背景移行通知は対象execution・解除理由・未実行取消件数という事実を渡し、人向け文言はFrontendが組み立てる。既存のin-process通知だけを使い、wire・保存event・実行状態は増やさない。
UIは既存のイベント・世代付き応答へリアクティブに追従し、表示用の時刻アニメーションも局所的に描画を要求する。
欠けた事実を作らず、未取得をidle、受付を完了、Turn完了をRun完了として表示しない。

履歴は不変DAGとThreadの採用経路を分け、summary/spillの出典と元の前後へ到達可能にする。
会話・回答・推論・要約は期限だけで自動削除しない。外部ファイルは参照し、履歴のためにコピーしない。
原文/anchor、保存済み割当、generation、取消しと遅着、永続互換の境界を維持する。

新しい型・表・event・worker・依存を加える場合は、既存責務では足りない理由と上限・終了責任を判断記録へ残す。
表数やソース行数だけでは過剰設計を判定しない。局所module分割や重複削減は通常のRust構成内で行う。

## 適用する値と契約

| 対象 | 適用する契約 | 根拠・影響 |
|---|---|---|
| TUIの観測経過 | 現在画面がTurnの活動を観測した単調時間。選択前・再起動前・未取得期間は含めず、復旧/Turn終端で停止。送信直後の時計は送信要求からのUIローカル経過 | [実行状態表示](tui-activity-indicator.md)、[UI](ui.md) |
| owner wait | 既定30,000ms、0は即確認、非負有限値。any、timeoutでも対象は生存 | [wait](wait.md)、[protocol](protocol.md) |
| 履歴semantic検索 | 既定10件、最大50件。ページcursorなし | [protocol](protocol.md)、[index](index.md) |
| 論理容量上限とsemantic閲覧 | 既存spaceの明示検索はquery embeddingを許可し、索引pauseを変更しない | [index](index.md)。新source/vector/jobの停止を維持し、queryの外部費用や物理disk/receipt別予算を免除しない |
| owner一覧 | 既定50件、最大200件 | [protocol](protocol.md)。cursorはscope/版/watermarkへ束縛 |
| owner本文取得 | 既定/最大64KiB、rawはraw bytesと専用decoder/cursor | [protocol](protocol.md)、[output](output.md) |
| docs検索/本文 | 検索既定10件・最大100件、本文既定16KiB・最大64KiB | [docs/RAG採用仕様](docs-and-rag-spec.md)。owner履歴検索の50件上限と混同しない |
| Thread session | 会話・要約のResponsesへThread UUIDのheaderを既定付与。本文prompt_cache_keyは任意・既定無効。embedding/一覧へ自動付与しない | [model HTTP](model-http.md)、[config](config.md) |
| 接続先/モデル | Responses proxy経由、利用可能モデル・context長・価格の内蔵カタログなし | [models](models.md)、[model HTTP](model-http.md)。特定proxy製品への実装依存なし |
| 再試行 | Failure-driven: no pre-POST estimate. First transient failure retries unchanged; eligible second failure reduces maximally. Explicit context overflow reduces immediately. All resends share the configured consecutive budget (default eight); success resets it. | [failure-driven reduction](failure-driven-reduction.md) |
| HTTP期限 | 接続確立は固定30秒。Responsesは要求開始からheader受領まで接続別120000ms（正整数）、header後の受信停滞600000ms（0で無効）、embeddingは全体60000ms、モデル一覧は全体設定と20000msの短い方 | [model HTTP](model-http.md)、[config](config.md)。timeoutは索引の既存retry/pauseへ渡す。生成全体期限は追加しない。会話の停滞timeoutは再試行条件に従う。要約/索引/RAGの再試行は変更しない |
| ストリーム完了 | 検証済み最終responseを採用。途中との差分は診断・保存だけとし、差分だけで復旧待ちにしない | [model HTTP](model-http.md)。ツールは最終responseの採用成功後だけ実行し、失敗時の受信本文も保存・参照可能にする |
| Unix IPC | XDGが利用不能でも専用0700一時領域を自動用意。接続両側でpeer uidを照合 | [protocol](protocol.md)。モデルshellはSHELLBOY_SOCKET、人CLIはendpoint.jsonで接続し、保存先祖先をsocket配置条件にしない |
| origin | 発行元の誤識別防止。OSユーザー隔離やpermission engineではない | [control](control.md)、[operation](operation-observability.md)。tokenを表示/モデル入力/保存結果へ複製しない |
| receipt | 観測はreceipt不要で再観測。変更/waitの同ID/同要求は保全中結果、異要求はconflict。24時間経過保守/圧迫で結果だけを回収し同IDを期限切れにする。pending/参照保護を維持 | [operation](operation-observability.md)。保存outcome8MiB/operation receipt256MiB、事前予約・旧terminal回収・receipt不要status照会 |
| AGENTS読込 | 新Turnごとに有限file workerで本文を読み直す。同Turn retryは保存済みsnapshotを維持。送信時だけ実snapshot pathを安全に示す1行を本文先頭へ付け、本文/hash保存・優先順位は変えない | [control](control.md)。mtime/size一致の内容変更も反映する。ファイルの原子的snapshotは保証しない |
| shell stdin EOF | 既知の閉鎖済みstdinへのEOF再送は冪等成功。未知IDやwriteの閉鎖済み入力への送信は別に検証 | [shell設計](shell.md)。acceptedは新規閉鎖・送信・shell終了の証拠ではない |

接続ごとの `ssl_verify` は既定true。明示falseだけHTTPS証明書検証を無効化し、会話・要約・embedding・モデル一覧へ一貫して適用する。

設定の正本は [設定設計](config.md) と `src/config.rs` の検証済みschema。
起動引数 > `SHELLBOY_DATA_DIR` > TOML > OS既定（接続CLIはTOMLを読まない）。
存在する不正設定を黙って無視しない。接続・モデル・embedding未設定は捏造せず、必要な操作だけを開始前に拒否する。
例示URLやmodel名は製品の既定接続ではない。

## 合意済み拡張と責務の置き場所

次の機能を採用する。設計の存在は、現在のsourceでの検証成功を意味しない。

| 採用対象 | 適用する設計 | 境界 |
|---|---|---|
| Markdown、コード強調、リンク、構造化出力 | [TUI表示](tui-presentation.md)、[popup共通基盤](tui-popup-framework.md) | OSSを利用。会話欄へ管理JSONを流さず、原文/完全値は別導線で保持 |
| 親子Thread一覧・一覧からの削除・タイトル・モデル選択・入力補完 | [Thread一覧](thread-browser.md)、[models](models.md)、[composer](composer.md) | 系譜とRun所有は別。Threadとモデルの取得/変更は既存操作を共用 |
| activityとスクロール | [activity](tui-activity-indicator.md)、[scroll](tui-scrolling.md) | 既存情報を投影し、Turn/Run/背景処理を区別。新しいコア状態は不要 |
| raw期限とcanonical検索 | [index](index.md)、[gc](gc.md) | raw期限/encoding変更はtool_rawだけを無効化し、保存済み引用/添付パスを期限で検索から消さない。旧欠落分の暗黙有料再構築はしない |
| 添付所属・順序 | [添付provenance](attachment-provenance.md)、[attachments](attachments.md) | 元source/要素位置を維持。後のmessageへ付替えない |
| 履歴filter・provenance・直接関係 | [検索と関係](search-and-relations.md) | DB順位付け前filter、L2距離、採用経路と生成元を区別。無制限DAG探索は追加しない |
| 削除影響・操作追跡・実行context | [全体](observability-and-deletion.md)、[削除](deletion-impact.md)、[操作](operation-observability.md)、[context](execution-context.md) | 読取りの判定材料。reclaimable容量や未記録時刻を捏造しない |
| docs・独立RAG | [採用仕様](docs-and-rag-spec.md)、[実装設計](docs-and-rag.md) | docsはowner/API不要。RAGは別store。履歴は明示read-only scope、生成モデルなし |
| 現在の作業状態・実行との関連・回復導線 | [work observability](work-observability.md) | 既存保存情報のprojection。statusはdurable_onlyでOS生存/独立RAGの統合snapshotではない |

PTY、MCP、permission engine、web UI、archive、成果物専用機構、pipeline、Windows本体からのWSL直接起動、
常駐する所有外Run、モデル価格カタログ、RAG内部でのLLM生成は採用対象に含めない。
将来一般的なskill対応と衝突させず、同梱ガイドのコマンド名は `docs` とする。

## 人向けの状態と復旧契約

通常画面は [User Guideの概念対応](../guides/user-guide.md#conversations-and-work-states) に従い、
内部IDを常に読み解く必要がない表示にする。詳しいID・原文・理由を失わない。
現在の処理と、過去に選択した管理結果の参照対象は別に保持する。古い結果を開いても現在の停止/復旧対象は変えない。

復旧待ちでは対象、理由、保全済み本文/受付、未確認の副作用、次の操作を示す。
同じTurnの明示retry、人間の新指示、停止を区別する。既存経路はF8のRun再試行、CLI `run retry`、
Ctrl+Cの現在Run停止、CLI `run stop`。TUIの `/retry` を現在の復旧待ちRunへのRunRetry、`/stop` を現在Threadの優先停止へ写す短い入口とする。
公開CLI契約・コア状態は増やさず、対象を別Threadの管理結果から継承しない。

設定変更や時間経過だけで復旧Turnを自動再開しない。通常の機械messageは人間の明示再開の代わりにならない。
再起動は実行の再開ではなく、前回の状態・不確実な結果を保存し、新指示で新Run/Turnを作る。
「retryではツールは絶対に再実行されない」と約束しない。確定済みtool resultは再利用し、不明な副作用を自動で再実行しない。

## 資源と受入の境界

有限queue・cache・出力・要求の予算は各対象設計と設定schemaに従う。cacheの合計をRSS上限と称さず、SQLite・allocator・進行中HTTP等を区別する。UIはDBやキュー空きを待たず、受理済み操作の所有と終了時の解放を維持する。

新しい公開操作・永続表・Core event・worker・queue・依存を加える場合は、既存責務では不足する理由、代替案、上限、所有者、寿命、終了時回収、互換を対象設計へ記録する。新しい汎用frameworkや状態機械を局所整理の目的で導入しない。

受入は [検証状況](../validation/acceptance.md) で実施と未実施を分ける。改名前の全件チェック、過去の私的ログ、合成mock、手順の記述を、現在のsourceの全環境成功として扱わない。強制終了後の孤児完全回収、外部ファイルの過去実体復元、全OS・モデル・介入順序の網羅は保証しない。実機のないOSや端末は未実施とする。


Issue #16〜#18の範囲選択・明示spill/summary・prefix/途中omit・復元は[圧縮と採用](compaction.md)に従う。共通のgroup境界検証と既存replacement/Thread経路を使い、原文を削除しない。Thread getの`context_usage`は直近成功usageと現在経路の概算を区別し、キャッシュhitや将来の受入可能性を保証しない。

Issue #19の削除確定と後片付け警告は[Thread一覧](thread-browser.md)、#20〜#22のsource除外・hybrid準備状態・独立RAGの状態投影は[Docs/RAG設計](docs-and-rag.md)、#23の抜粋制限とpayload_charsは[履歴検索設計](search-and-relations.md)に従う。独立RAGの除外列以外に表を増やさず、会話Coreの操作・worker・event・queueは追加しない。

Issue #24の内部形式変換の廃止と原本付きingestは[Docs/RAG](docs-and-rag.md)、#25/#26の末尾採用・CAS・DB v10の索引は[経路とhot query](history-adoption-performance.md)、#27/#28の有限readonly検索と出力通知は[readerとwait](history-search-readers-and-waits.md)に従う。新しい一次情報の表や汎用pool/ジョブengineは作らない。独立RAGはschema 3へ前進し、会話DBのmigrationとは分ける。

Issue #29/#30のrender-unit再利用、可視範囲の幅再描画、有限outboxとUI専用購読は[描画・配送設計](tui-render-delivery-performance.md)に従う。layout予算は既存合計を維持し、重要結果は最新状態へ集約しない。新しい永続表/worker/event型は作らず、有限UI channelの所有とpriority終了経路を明示する。

English is the default documentation language. Unsuffixed guides and the bundled docs catalogue are English; only the User Guide has a Japanese translation (`user-guide.ja.md`), and the Agent Guide is English only. See [documentation language and the UI evaluation](documentation-language.md). The UI remains unchanged; the bilingual UI recommendation is a future implementation proposal, not a current configuration contract.

Issue #34 adds early maximum safe reduction, an explicit-hint-only pre-POST
check, and keyword `rag search --scope history:THREAD`. Semantic history and
scoped RAG reuse the selected-path plus completed-replacement-source scope.
Membership in that scope and membership on the current selected path remain
separate. Original nodes are not deleted. Automatic reductions have no numeric
target (`target_tokens: null`); manual edits retain their explicit target.

HTTP attempt diagnostics record body-source consumption, header elapsed time,
receive phase and known pool generation. Source consumption is not proof of TCP
write completion or peer receipt; exact socket reuse remains unknown. A failed
pool generation is retired for future requests, without claiming immediate
closure of existing concurrent HTTP/2 requests. Repeated known-incomplete body
sources stop automatic retries. No new persistent state, table, worker, queue
or dependency is introduced.

Issues #44/#45 share the saved activity classifier between the paged Thread
list and conversation footer, and expose read-time live-shell duration plus
latest retained output time on shell get/status. Native lifecycle state and
terminal monotonic duration are unchanged. See [Thread browser](thread-browser.md),
[shell](shell.md) and [work observability](work-observability.md). No table,
worker, poller or migration is added.

Issue #46 adopts [measured CI test performance](ci-test-performance.md): preserve
all native test identities and assertions while reducing repeated integration
linking and fixture-only commits. Keep native timing evidence distinct from
policy tests and do not add product machinery for the test runner.

Issues #42/#43/#55 adopt [index operations](index-operations.md): saved pause
observations and recovery UI, deterministic bounded embedding batches, and
explicit dimension rebuild/probe using existing spaces/app_meta/workers.

Issue #51 adopts [explicit instruction references](agents-reference.md): one
reference per Thread, strict explicit-path validation, atomic settings and
next-new-Turn notification without altering same-Turn snapshots.

Issue #49 adopts [standard usage measurements](usage-accounting.md): persist
validated standard numbers only and read legacy large usage through guarded SQL
scalar extraction. Task-boundary cleanup is Agent Guide guidance, not Core automation.


Issue #53 adopts [interrupted message recovery](message-recovery.md): persist
path-admission facts, share terminal repair, and recover known unadmitted input
only at an authorized explicit resume. Ambiguous historical admission remains
visible and requires explicit restoration; original assignments remain immutable.
Issue #56 adopts [tool cancellation reasons](tool-cancel-reasons.md): distinguish
failed prerequisites from released waits using saved facts without changing
execution policy or adding an automatic background timer.

Issue #52 adopts [pending messages and human Turn interruption](pending-messages.md):
one reactive saved queue with paired history reads, per-message cancellation,
durable draft correlation, explicit foreground release and a Turn-only human
interrupt. Full Run stop remains separate; automatic timed backgrounding is deferred.

Issue #47 adopts [saved usage totals](usage-aggregates.md): indexed read-time
SQL over saved attempt states/standard usage and saved Run/model/shell timing,
first measured at synthetic scale. All-time only; unrecorded summary/embedding
coverage is explicit. No schema, producer observation or persistent totals are
added unless measured slowness justifies a separately approved minimal follow-up.

Issue #58 adopts [reactive history retention](tui-history-retention.md) before
the independently reviewable conversation redesign increments tracked in
[the #41 plan](../development/issue-41-plan.md). Follow ownership, finite visible
working sets and paid-text hydration remain invariants throughout those changes.

Issue #41 adopts [conversation redesign](conversation-redesign.md) in independently
usable increments following #58. The delivery ledger includes G operation/forms
requirements added through6519745314. Adoption does not claim all increments implemented.

## #50: factual reduction

[Failure-driven reduction](failure-driven-reduction.md) supersedes the estimated
75k/hint preflight and once-per-Turn policy in older compaction/retry sections.
Only failure facts activate automatic reduction; all resends share the normal
consecutive retry budget. Context edit sizes are characters, never token estimates.

## Message sender facts (#59)

Adopt [sender inspection](protocol.md#message-sender-inspection-59): expose
credential-free receipt provenance and optional bounded self-asserted labels.
Absent historical evidence stays unknown; new external CLI is not called human.
Labels have no authority. No schema migration or conversation rendering change.
