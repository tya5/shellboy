# 設定契約と実装照合

採用設計。設定値・検証・パス解決の現行契約を本書に示す。`src/config.rs` と `tests/config.rs` を対応成果物とする。初期設定資料は非同梱。

`Config` は公開TOML全節の型。各節で `deny_unknown_fields` を適用し、未指定は節単位・フィールド単位とも既定値にする。`compaction.concurrency` は公開設定ではないため受理しない。内部の要約同時実行数1は実行制御側の責務。接続なし、modelなし、embeddingなしを有効な既定状態とし、架空の接続を作らない。モデル/effortの固定カタログを設けない。

数値は1024基数のbaselineを採用。正整数/非負整数、file <= request、warning < max、batch <= buffer、retry遅延個数と正値を検証する。UIキーは修飾子Ctrl/Alt/Shift＋文字または端末キー名を受理し、send/newlineの衝突を拒否する。Enter送信、Ctrl+JとShift+Enter改行が既定。

`Config::parse` は純粋な解析・検証。`Config::load` / `load_with_env` / `load_with_overrides` はblocking I/Oを含み、UI開始前または有限file worker上で呼ぶ。明示ファイルのNotFoundはエラー、既定ファイルのNotFoundだけ組込み値に戻す。存在する不正ファイル・読取り失敗を黙って無視しない。`LoadedConfig` は解決したdata-dirと実際の設定ファイル位置（既定ファイル不存在ならNone）を保持。

`PathEnvironment` はOS・環境パスの注入境界。owner用 `resolve_data_dir` はCLI > SHELLBOY_DATA_DIR > TOML > OS、接続CLI用 `resolve_cli_data_dir` はCLI > SHELLBOY_DATA_DIR > OS。接続CLIはTOMLを読まない。CLI/環境相対data-dirは起動cwd、TOML相対data-dirとshell executableは設定ファイル親へ一度絶対化する。OS既定環境パスは絶対パスを必要とし、解決不能時はエラー。存在しない保存先も扱うためcanonicalizeは行わず、directory作成/lockは起動層が行う。Unixホーム、XDG、macOS Application Support、Windows APPDATA/LOCALAPPDATAは [設定ガイド](../guides/configuration.md) のOS別パスを明示実装し、外部ライブラリ既定に置換しない。

`Connection::endpoint` はHTTP(S) API rootからresponses/embeddingsを一度だけ連結。終端API名、URL内資格情報、query、fragmentを拒否し、URLの推測修正を行わない。固定headersのHTTP名/値、case-insensitive重複、Authorizationとapi_key_envの二重定義を検査する。資格情報取得は `credentials` で要求開始直前に遅延実行し、空/不存在ならエラー。接続、shell env、解決済資格情報のDebugは機密値を伏せる。TOMLエラーには入力本文の抜粋を表示しない。

実装判断: env変数名は移植性のあるASCII英数字/underscore（先頭は英字/underscore）へ制限した。追加headerの空値はHTTP上許容し、API keyの空値は拒否する。明示URL内資格情報を禁止する理由は、秘密がURLを通じてログへ現れることを防ぎ環境変数指定へ統一するため。shell executable未指定はNoneとしてOS shell adapterの候補解決へ渡し、起動時に別shellへfallbackしない。これらは初版契約の具体化として採用し、特殊な環境変数名を実proxy運用で必要とした場合に制限を再確認する。

検証: `cargo test --test config` は既定値、全節の未知キー、型違い、数値関係、接続参照、URL/認証矛盾、秘密伏せ、パス優先順位、明示/既定設定ファイルを対象とする。OS注入によるパス試験は各OSでの実機動作検証を代替しない。shell実行、lock、実proxyはこのモジュールの試験範囲外。

## Thread単位のセッション識別

`connections.<名前>.session_header` はboolで既定true。会話と要約のResponses要求に、対象Thread UUIDだけを値とする `x-shellboy-session-id` を自動設定する。同じThreadの追加要求・再試行・モデル切替でも同じ値を使い、子Threadとforkは各自のThread UUIDを使う。会話本文・タイトル・cwd・ユーザー名は識別子へ含めない。embeddingとモデル一覧には自動付与しない。

自動設定がtrueの場合、Responses要求では大文字小文字を問わず同名の固定 `headers` 値をThread UUIDで置き換える。ほかの固定headerを変更せず、固定headerの検証規則も変えない。falseは自動付与だけを停止するため、利用者が `headers` へ明示した同名headerは残る。embedding・モデル一覧でも固定headersは従来どおり適用する。

`connections.<名前>.prompt_cache_key` はboolで既定false。明示trueの場合だけ、会話と要約のResponses要求本文へ同じThread UUIDの `prompt_cache_key` を付ける。falseならこの自動設定項目を本文へ追加しない。providerがこのフィールドを受理する場合に有効化する。キャッシュhitや同一上流への接続、障害解消を保証しない。

両値ともRust DefaultとTOML省略時を揃え、文字列・整数などbool以外を拒否する。owner再起動後の要求へ反映する。LiteLLM側のroutingでは `x-litellm-session-id` やtrace ID等が優先される構成があるため、意図せず全Thread共通の固定識別headerを併用しない。接続先側の設定・優先順位は [設定ガイド](../guides/configuration.md#per-thread-session-identity) と [model HTTP](model-http.md) を参照する。

## 接続ごとのTLS検証

`connections.<名前>.ssl_verify` はboolで既定true。明示falseの場合だけ、その接続のHTTPS証明書検証を無効化する。HTTP APIへの直接接続では検証対象がない。HTTPS中継プロキシを経由する場合は、そのプロキシ証明書にも同じ設定を適用する。ConnectionのRust DefaultとTOML省略時をともにtrueへ揃え、文字列や数値は拒否する。ownerは起動時の設定を保持するため変更後は再起動する。独立RAGは実行時に読む同じ接続設定を使う。接続CLIからownerのTLS設定を上書きしない。

## 接続ごとのHTTP待機期限

`connections.<名前>.stream_idle_timeout_ms` は非負整数で既定600,000ms。会話・要約のストリーミングResponses要求で、応答header受信後の各chunk間の無通信時間を制限する。0はbodyの無通信期限だけを無効化する。headerは別のresponse_header_timeout_msで制限する。受信が続く間の全体時間に上限を置かず、SSE heartbeatも受信として扱う。

`connections.<名前>.request_timeout_ms` は1,000以上の整数で既定60,000ms。embeddingなど非ストリーミングHTTP要求の全体期限に使う。モデル一覧は同設定と20,000msの短い方を適用する。両設定は接続ごとに独立し、Rust DefaultとTOML省略時の値を揃える。負数・文字列・小数・範囲外整数は拒否する。設定変更はowner再起動後の要求へ適用する。期間の加算はHTTP側でchecked操作を行い、OSの時計で表現できない期限をpanicや即時timeoutへ変換しない。

## 接続ごとの会話再試行

`connections.<名前>.response_max_retries` は0以上20以下の整数で既定8。会話Responses要求の連続する一時失敗に対し、初回要求に加えて行う自動再試行回数で、0は無効にする。最終responseの検証・保存・採用に成功した時点で連続失敗予算をリセットする。単なる受信開始やcontext超過の圧縮ではリセットしない。

`connections.<名前>.response_retry_max_delay_ms` は1以上300,000以下の整数で既定60,000ms。指数バックオフによる1回の待機時間の上限とする。サーバーの `Retry-After` はこの設定と別に固定600,000msを上限とする。`connections.<名前>.response_retry_partial` はboolで既定true。生成本文・tool引数を一部受信した通信切断、EOF、受信停滞timeoutも再試行対象にし、falseならそれらは復旧待ちにする。metadataやheartbeatだけで出力をまだ受け取っていない場合はfalseでも一時的な通信失敗の対象とする。Rust DefaultとTOML省略時を揃え、負数・文字列・小数・範囲外整数を補正せず拒否する。owner再起動後の要求へ適用する。 Retry-Afterが600秒（10分）を超える場合、600秒に短縮して再送せず、指定待機時間を保存・表示して復旧待ちにします。

設定の消費者は会話Driverだけとし、HTTP層の透過再送、要約、embedding、モデル一覧には適用しない。HTTPエラー本文は生成出力と区別し、認証・入力形式・明示provider失敗など恒久エラーを自動再送しない。部分応答を再試行するときも失敗attemptの受信本文は保存する。未採用のtool callは実行せず、停止は再試行待機を取消す。自動再試行は上流の未処理を保証せず、重複課金の可能性をなくす設定ではない。判定と実行順序は [model HTTP](model-http.md) に従う。

## TUIの通知ベル

`ui.palette` accepts `dark` (default) or `light`. It selects the fixed header
and user-message surfaces without changing source text, Markdown semantics or
saved state. Resolve `NO_COLOR`, `TERM` and `COLORTERM` once when constructing
the UI: nonempty `NO_COLOR` selects plain markers/spacing, known 256-color or
truecolor capability selects the configured palette, and other terminals use
reverse/separator fallback. Do not query terminal background or perform terminal
I/O to infer colors. Invalid palette strings/types are configuration errors.

`ui.bell` はboolで既定true。TUIが起動後に新たな復旧待ちを観測したとき、現在会話と背景会話のどちらも端末BELで通知する。起動時に既に存在する復旧待ちは通知せず、同じ復旧待ちの再取得・画面切替・再描画でも鳴らさない。複数の通知は2秒間隔へ集約し、ベル専用worker・永続event・保存状態を追加しない。既存UI通知からローカルに判断し、falseなら通知を無効化する。音・visual bell・画面flashの実際の表現は端末とmultiplexerの設定へ委ねる。詳細は [画面操作](../guides/tui.md#recovery-wait-notification-bell) に示す。

## R3-6: 設定検証の局所化

Connection内のURL/effort/credential名/header検証はConnectionへ、数値・範囲・retry整合・env・path/keyは各sectionへ移す。Configは既定接続とembeddingの参照関係を持つ。first errorを維持するため、数値positive→worker容量→local相互条件→connection内容→section間参照→env/path/keyの順序を明示し、単にsectionごとの一括検証へ並べ替えない。Ui/Shellなどは既存の段階に合わせた小さな検証関数を持つ。

positive指定の共通化は全fieldが正の数値であるRetention/Limits/Io/Workers/Loggingに限定し、既存section!へ小さなall-positive armを加えて宣言順で検証する。Cacheの0許可、Indexing.retries=0と空delay、mixedなUi/Indexingは明示検証のまま保持し、schema DSLや汎用rule registryへ拡張しない。公開TOML/default/未知field拒否/上限/secret非表示は変えない。

## 起動進捗と初期化の取消（P8）

TUIは一つの端末sessionを先に開き、設定/保存先・owner lock・DB版検査/migration・前回中断整理・Control・接続口の準備を非永続stageとして表示する。進捗は最新1値、結果は1件の有限channelで受け取り、UIはtry_recv・短いstage参照と端末eventだけを扱う。設定/cwd/file処理は既存blocking経路、migrationと中断整理は既存DB worker、残る初期化は既存async runtimeで実行する。新Core業務state・DB表・event・workerは足さない。stageは完了率・処理生存の推測ではなく、開始した処理の表示だけである。

設定ファイルのdata_dir（TOML親基準）をCLI > SHELLBOY_DATA_DIR > TOML > OSの優先順位で解決するには、設定の読取り・純検証がlockに先行する。初期提案のlock→設定検査という順序は「解決前に不明なdata-dirをlockする」とは解釈しない。解決済み保存先のlock後だけDB版検査・migration・書込みを行い、設定不正ならDBを作り直さず停止する。接続CLIは従来どおりTOMLを読まない。

初期化中に会話・slash command・貼付入力は受け付けない。Esc/Ctrl+C/Ctrl+Qは終了意図だけを通知し、開始済みblocking処理・migration・中断整理をabortしない。安全な初期化境界まで待ち、準備済みControl・transport・DBを順に閉じ、実際のwriter終了までowner lockを保持する。強制完了や即時lock解放を宣言しない。結果配送と取消が競合しても、UIへ通常運用を渡す前に終了意図を再検査し、必要なら同じ起動画面でcleanup完了を待つ。RPCのCore serveは通常UIへのhandoff後に開始し、準備中に公開されたtransportで要求が届いてもCoreへ受付しない。

失敗は既存のtyped ApiErrorを人向け表示し、Enter/Esc/終了キーで確認後に端末を戻す。初期化失敗後のDB writerは閉じてから結果を返す。端末自体が壊れた場合は端末sessionを終了してから初期化/cleanupの完了を待ち、成功として扱わない。起動画面はUI-onlyかつ一回限りで、会話下書きや管理popupの状態へ混ぜない。

取消単位は既存DataOwner初期化全体とし、途中futureをdropしない。既にこの単位へ入った場合、終了要求後もlock取得・migration・中断整理まで完了しうる。migration済みの変更を巻き戻したとは表示しない。その後のControl開始・接続口公開は次の取消検査で止める。これ以上細かい取消APIやCore状態を増やさず、実writerとlockの寿命を一貫させるための選択である。



## Responses header deadline (Issue #34)

`connections.<name>.response_header_timeout_ms` is a positive integer, default
120000. It bounds the request-start-to-response-headers interval, including upload,
independently of post-header stream idle time. Zero is rejected; disabling body
inactivity with stream_idle_timeout_ms=0 does not disable this deadline. Owner
restart applies configuration changes. Embedding/catalog total limits do not change.

## Optional Revisit consumption limit (#36)

`limits.revisit_max_per_run` is an optional unsigned integer: absent means no
consumption limit, zero disables new registrations, and a positive value caps
consumptions per Run. Other Limits fields remain strictly positive. The small
section declaration macro distinguishes optional fields from the existing
positive numeric fields; it does not add a validation registry. See
[Revisit](revisit.md) for the `revisit_limit_reached` rejection and counter.


## Deterministic history embedding batches (#43)

`embedding.batch_max_inputs` defaults to 1 and accepts 1..128.
`embedding.batch_max_chars` defaults to 64000 and accepts 1..1048576 Unicode
characters. For multiple inputs it must cover `indexing.split_chars`. The
default preserves singleton requests; configured limits are user-selected
bounds, not inferred provider limits. Requests share one space/request mode
and a stable saved-work ordering. Queries remain singleton. No dimension
setting is added; explicit rebuild mode is saved with its space. See
[index operations](index-operations.md).

## Explicit instruction references (#51)

The [per-Thread instruction reference contract](agents-reference.md) supersedes
previous fixed-reference wording: explicit create/set/fork selection is allowed,
while cwd changes alone never rediscover instructions and same-Turn retry
continues using its durable snapshot.

#50 removes compact_hint_tokens (unknown-field rejection, no alias). Existing
auto_trim_on_disconnect gates failure-driven reduction, never pre-POST policy.
See failure-driven-reduction.md.

## First-run setup (#69)

`config init` and `config check` are standalone noun/verb CLI commands, like
`thread create` and `model list`, not owner RPCs. Init exclusively creates the
selected configuration file from the bundled `config.example.toml`; it never
overwrites a file or symlink, prompts for keys, or starts an owner/database.
Check loads the same strict configuration and path rules, reports missing
connection/model/credential-variable names without values, and only sends one
bounded GET model-catalog request through the existing HTTP client. A model
absent from the catalog is a warning with exit success, not proven unusable.
HTTP 404 and endpoint-suffix errors explain API-root syntax (`.../v1`, not
`.../v1/responses`). No Responses/embedding request is sent by check.

Loaded configuration carries ephemeral source-path/presence metadata even when
the default file is absent; it is not a TOML field or persistent schema change.
The existing empty-conversation welcome and submission/recovery notices project
local setup facts and next actions, with Japanese TUI text. No new dialog,
conversation renderer, automatic file creation, network probe at startup or
per-command approval mechanism is added. Configured users see no setup notice.

The exact bundled example model/API-root placeholders are reported as unfinished
setup too; syntactically valid template values are not presented as ready.

Setup command paths must be Unicode so JSON diagnostics cannot panic after a
file-system error or file creation. Reject unsupported config paths before I/O;
diagnostic data-directory paths use display-safe UTF-8, without changing their
filesystem resolution. This adds no credential value to diagnostic output.

## Terminal program status (#54)

`ui.program_status` is `"on"` (default) or `"off"`. OSC7501 rev0.4 is emitted
without querying the terminal, using the existing UI writer between frames.
No conversation/status-row renderer, lifecycle state or Core table is changed.
See the terminal-status design for selection, limits and lifetime semantics.

## Automatic conversation indexing (#71)

`indexing.auto_history` is a strict boolean, default true, independent of the
embedding connection/model. False disables automatic history authorization and
holds automatic pending work; explicitly selected indexing and standalone RAG
remain available. Restart the owner to change the immutable runtime setting.
