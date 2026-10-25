# 適用仕様

Shellboyの開発で使う採用仕様の入口です。仕様の採用と実装・検証の完了を区別します。現在地は [status](../development/status.md)、改名後の確認範囲は [acceptance](../validation/acceptance.md) に記録します。

## 読む順序

1. 本書で対象範囲と境界を確認する。
2. [Control](control.md) と [状態境界](control-boundary-map.md)、対象領域の [採用設計](README.md) を読む。
3. 操作は [利用ガイド](../guides/README.md)、公開wireは [protocol](protocol.md)、CLIは [cli](cli.md)、設定は [config](config.md)、保存は [storage](storage.md) と配布migrationを照合する。
4. 公開型・設定schema・migration・fixtureと設計の不整合を解消してから実装する。

このリポジトリ内の採用設計を開発基準とし、非公開資料や過去レビュー台帳を前提にしない。同じ事項の変更は対象設計と本書を整合させる。文書の日付・レビュー提案・現状コードだけで契約を変更せず、理由と影響を [判断記録](../development/decisions.md) に残す。

Shellboyへの改名では旧名の実行名・環境変数・既定保存先への互換を提供しない。旧データの移動は不要であり、既存環境を変更しない。新しい既定保存先は [setup](../development/setup.md) に記載する。

## 変えない責務

Rust / Ratatui / Crossterm / Tokio / bundled SQLite / sqlite-vec を使い、依存版は固定する。
UIのメインOSスレッドは描画・入力・メモリ上の状態更新を担当する。I/O、DB、join、キュー空き待ちは置かない。
共有async runtime、専用DB worker、有限blocking file workerを使う。
モデルのツールはshellだけで、履歴・送信・子・待機・添付の操作は同じ `shellboy` CLIへ集める。

Controlが受付・順序・所有を管理し、Storeが短いtransactionで保存し、Driverが要求を進める。
Frontendは保存済み情報から人向け表示を作る。別のジョブエンジン、常時監視poller、汎用遷移DSLを足さない。
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
| owner wait | 既定30,000ms、0は即確認、非負有限値。any、timeoutでも対象は生存 | [wait](wait.md)、[protocol](protocol.md) |
| 履歴semantic検索 | 既定10件、最大50件。ページcursorなし | [protocol](protocol.md)、[index](index.md) |
| 論理容量上限とsemantic閲覧 | 既存spaceの明示検索はquery embeddingを許可し、索引pauseを変更しない | [index](index.md)。新source/vector/jobの停止を維持し、queryの外部費用や物理disk/receipt別予算を免除しない |
| owner一覧 | 既定50件、最大200件 | [protocol](protocol.md)。cursorはscope/版/watermarkへ束縛 |
| owner本文取得 | 既定/最大64KiB、rawはraw bytesと専用decoder/cursor | [protocol](protocol.md)、[output](output.md) |
| docs検索/本文 | 検索既定10件・最大100件、本文既定16KiB・最大64KiB | [docs/RAG採用仕様](docs-and-rag-spec.md)。owner履歴検索の50件上限と混同しない |
| 接続先/モデル | Responses proxy経由、利用可能モデル・context長・価格の内蔵カタログなし | [models](models.md)、[model HTTP](model-http.md)。特定proxy製品への実装依存なし |
| 再試行 | 明示context超過のみ自動圧縮最大2段階。未知失敗で自動再送しない | [compaction](compaction.md)、[recovery guide](../guides/recovery-and-storage.md) |
| origin | 発行元の誤識別防止。OSユーザー隔離やpermission engineではない | [control](control.md)、[operation](operation-observability.md)。tokenを表示/モデル入力/保存結果へ複製しない |
| receipt | 同ID/同要求は保存済み結果、異要求はconflict。現instance結果を勝手に期限回収しない | [operation](operation-observability.md)。保存outcome8MiB/operation receipt256MiB、事前予約・旧terminal回収・receipt不要status照会 |
| AGENTS読込 | 新Turnごとに有限file workerで本文を読み直す。同Turn retryは保存済みsnapshotを維持 | [control](control.md)。mtime/size一致の内容変更も反映する。ファイルの原子的snapshotは保証しない |
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
| 親子Thread一覧・タイトル・モデル選択・入力補完 | [Thread一覧](thread-browser.md)、[models](models.md)、[composer](composer.md) | 系譜とRun所有は別。Threadとモデルの取得/変更は既存操作を共用 |
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

通常画面は [User Guideの概念対応](../guides/user-guide.md#会話と作業状態の読み方) に従い、
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
