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
`connections.<名前>.session_header_names` は文字列の配列で既定 `["X-Shellboy-Session-Id", "x-litellm-session-id"]`（最大8個）。`session_header=true` のとき、各名前で同じThread UUIDを送る（付与範囲は同じ）。後者はLiteLLMが読むsession headerで、未知のheaderは他providerが無視する。コードにgateway固有の分岐は置かず、既定値だけに含める。利用者は配列を上書きして外す。空配列は何も送らない。各名前は `extra_headers` と同じ規則（header名の文字・256バイト以内、資格情報風とtransport所有の名前を拒否、配列内の大文字小文字を無視した重複を拒否）で検査し、`session_header=true` の間は `headers`・`extra_headers`・資格情報headerとの重複も拒否する。旧既定名 `X-Shellboy-Session-Id` は `headers` との同名を従来どおり許可し、Thread UUIDが上書きする。

自動設定がtrueの場合、Responses要求では大文字小文字を問わず同名の固定 `headers` 値をThread UUIDで置き換える。ほかの固定headerを変更せず、固定headerの検証規則も変えない。falseは自動付与だけを停止するため、利用者が `headers` へ明示した同名headerは残る。embedding・モデル一覧でも固定headersは従来どおり適用する。

`connections.<名前>.prompt_cache_key` はboolで既定false。明示trueの場合だけ、会話と要約のResponses要求本文へ同じThread UUIDの `prompt_cache_key` を付ける。falseならこの自動設定項目を本文へ追加しない。providerがこのフィールドを受理する場合に有効化する。キャッシュhitや同一上流への接続、障害解消を保証しない。

`connections.<名前>.prompt_caching` はboolで既定true（#127、opt-out）。共通のキャッシュの目印を方言のcache markerとして送る。`anthropic_messages` だけが実際に何かを送り、`responses` と `chat_completions` では新しく何も送らない。falseではcache関連を何も送らない。`prompt_cache_key=true` と `prompt_caching=false` の組合せは矛盾として拒否する。配置は [model HTTP](model-http.md#common-cache-hint-127) を参照する。

これらの値はRust DefaultとTOML省略時を揃え、文字列・整数などbool以外を拒否する。owner再起動後の要求へ反映する。LiteLLM側のroutingでは `x-litellm-session-id` やtrace ID等が優先される構成があるため、意図せず全Thread共通の固定識別headerを併用しない。接続先側の設定・優先順位は [設定ガイド](../guides/configuration.md#per-thread-session-identity) と [model HTTP](model-http.md) を参照する。

## 接続ごとの追加指定（extra_body / extra_headers、#119）

提供元ごとの任意の指定（推論の細かい設定、サンプリング、キャッシュ指定、ベータheader等）はShellboyが解釈しない。`connections.<名前>.extra_body`（TOMLの表、JSONとして要求本文の最上位へ追加）と `extra_headers`（文字列の表）を、会話・要約の要求にだけ付ける。未設定の要求は従来と完全に同じ。検証はJSONで表せること（TOMLの日時を拒否）、大きさ（本文64KiB、header 32個・値8KiB）、Shellboyが持つ欄との衝突の3点だけ。予約欄は方言ごとの一覧（`Api::reserved_body_fields`）で、衝突は上書きせず拒否する。そのため `reasoning`／`reasoning_effort` は `default_effort` が持ち、`extra_body` との優先順位は存在しない。`extra_headers` は資格情報らしい名前（authorization、cookie、名前に api-key／secret／token／credential を含むもの）と転送を所有するheader（host、content-*、accept、`x-shellboy-session-id`）を拒否し、`headers` と大文字小文字を無視して重複できない。attemptの入力manifestには追加指定の正準JSONのSHA-256（`extra_digest`）だけを残し、中身は設定ファイルが持つ。再起動で反映する。

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

## Revisit has no limit setting (#187)

`limits.revisit_max_per_run` (#36) is removed and is now an unknown key. Revisit
is an ordinary message bounded by the existing message limits; see
[Revisit](revisit.md#limit-decision).


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

## Motion (#90)

`ui.motion` is `"on"` (default) or `"off"`. It is display-only: off removes every
animation deadline (status shimmer/pulse, conversation spinner, owned-work
heading blink) and draws the static marks; meaningful seconds still update.

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

## Automatic indexing kinds (#254)

`indexing.auto_kinds` is an array of node-kind words, default
`["user","summary","assistant_response"]`, each one of `user`, `summary`, `assistant_response`,
`tool_result`, `spill`, `event`; unknown words are `ConfigInvalid` with key
`indexing.auto_kinds`, an empty list is valid. It is read at owner start. The
words are the node kinds `history search --kind` already accepts, so a typical
user has one vocabulary. `config check` and `index status` report it. See the
index design for where it is applied.

## Explicit summary limits (#73)

The optional `[compaction]` section follows D-015 and the
[bounded summary contract](compaction.md#bounded-explicit-summaries-73--d-015).
Unlike sections with independent defaults, an explicitly present section uses
only its specified limit keys; omission of the whole section selects both
20,000 characters and 5%. Config parsing and config check enforce the same
numeric validation and expose resolved limits without a paid conversation call.

## Responses cache diagnostics (#142 part 1)

Adopt [cache diagnostics](cache-diagnostics.md):
`connections.<name>.prompt_cache_diagnostics = false` by default; true is
Responses-only and requires `prompt_caching`. The owned dynamic
`prompt_cache_options` field is reserved against `extra_body`. No sticky headers
or provider-specific settings are introduced.

## Reading and changing through the owner (#266)

`src/config_edit.rs` owns the file work and `src/control/config_ops.rs` runs it on
`AttachmentService::control_files()`, the existing bounded control-file pool that owner shutdown already
waits for. No new worker, table, event or queue. `config_write` (a Tokio mutex on `Control`) admits one change
at a time; the worker closure owns its guard, so a caller that timed out does not let the next change start
while the first is still running. The UI thread and the DB worker do no file I/O for either operation.

**What leaves the owner.** `fields()` is the single list, for both reading and diffs. `Config`, `Connection`
and `Shell` are destructured without `..`, so a new field does not compile until it is classified. The other
sections are serialized whole, and the macro that declares them says so.

| Class | Fields | Returned |
|---|---|---|
| Secret | `connections.*.headers`, `extra_headers`, `shell.env` (each entry), `extra_body` (each top-level key, whatever is below it) | The name, value `[redacted]`. A name that itself looks like a credential appears as `[redacted-name-<8 hex>]`, from a digest so it stays distinct and stable |
| Url | `connections.*.base_url` | Userinfo, query and fragment removed (validation already refuses them); a path segment that looks like a credential becomes `[redacted]` |
| Name | `connections.*.api_key_env`, `api_key_header` | The name; `[redacted]` if it looks like a credential (a valid variable name can be `ghp_...`) |
| Plain | Everything else | The value; a string with a known credential prefix becomes `[redacted]` |

Everything hidden is listed in `redacted_fields`. Ordinary names of headers, variables and `extra_body` keys
are returned. Connection names are always returned, because Threads refer to them. `credential_env_set` is presence in the owner's environment only. `content_hash` is a SHA-256 of
the whole file, secrets and comments included: it identifies a version and is not a salted digest, so a
reader could confirm a guess of the entire file. An invalid file returns its validation message, which is
built not to quote values, and no values.

**Writing.** Read and hash; compare with `expected_hash`; edit with `toml_edit`; parse the whole result with
`Config::parse` (the same rules `config check` applies when loading; the catalog GET and the setup report are
not repeated); build and bound the result; write a temporary file in the target's directory, keep the mode on
Unix, `sync_all`; hash the file again; replace. The temporary file is created `0600`. A failure removes it and
leaves the file as it was; a crash can leave a `.config.toml.<uuid>.tmp` beside the file.

- A symlink resolves to its target, so a linked dotfile stays linked. A read-only file is refused.
- Unix replaces with `rename`. Windows uses `ReplaceFileW` for an existing file, which keeps its ACL and
  attributes. It takes no backup name, so its documented failures fall in two groups:
  - Errors 1176 and 1177 mean it stopped midway and nothing is at the path. After 1176 the previous file is
    gone; after 1177 it survives under a name Windows chose. The temporary file may be the only copy, so it is
    kept and moved into place. If that works, the result has a `warning`, since the permissions may differ. If
    not, the error has `details.reason = "replace_interrupted"` and `details.recovery_file`.
  - Every other error leaves both files under their own names; the temporary file is removed and the error says
    the file is unchanged.
  The recovery runs on every OS with the failure simulated. A real 1176 or 1177 has not been produced; the
  Windows test reaches a real `ReplaceFileW` failure through a sharing violation, which is the second group.
- The last hash shrinks the race with another program's edit to the single replace step. It does not remove
  it: an edit landing between that read and the replace is lost. The mutex is per `Control`, so changes through
  one owner are serialized, while two owners sharing a file (different data directories, one config) only get
  the narrowed race, like any other program.
- The owner's Unix user becomes the file's owner if it differs; ownership is not restored.

**Comments.** Untouched text is preserved byte for byte, and a consistently CRLF file stays CRLF. A file mixing line endings comes back with LF, because the parser normalizes them. A replaced value keeps its trailing
comment. A removed key's comment lines above it move to the next key, or before the next table, or to the end
of the file: they often describe more than that key. A trailing comment on the removed line goes with it, and
so does anything in a replaced multi-line array. Removing a connection removes its table, its subtables and
the comments attached to them.

**What can be changed.** Any key `Config::parse` accepts, plus these rules: `data_dir` is refused because after
a restart the owner would open another, possibly empty, directory; a whole table cannot be replaced or removed
except adding a new `connections.<name>` from an object and removing one. A credential-looking string in
`api_key_env`, `api_key_header`, `session_header_names` or a `shell.env` name is refused without echoing it.
So is one in any part of a field path or any key of an object value, reported by the parent's name so the
refusal doesn't repeat it. The check is a heuristic (known key shapes with a digit, full AWS and Google key
shapes, long opaque alphanumeric tokens), so its error says how to proceed by hand when a real name is caught.
Removing such a key stays allowed, as the way to clean one up. A value or field containing `[redacted` is refused
(`redacted_placeholder_refused`), since a form posting back what `config.get` showed would destroy the secret. A
result over the 1 MiB read bound is refused before the replace, since it could not be read back. Values may be written to `headers`, `extra_headers`, `extra_body` and `shell.env`,
because those fields hold them today; they then appear only as `sensitive` entries.

**Restart and live connections (#269 stage C).** `default_connection`, `default_model` and connection definitions
apply to the next request without restarting. The connection named by the running `embedding.connection` stays at
its startup definition until restart, preserving the index profile key. All other settings, including `embedding.*`,
`compaction.*` and `retention.*`, retain restart semantics. Standalone commands that load the file themselves see
changes on their next invocation. Direct file edits do not reload the owner; use `config.set` for live application.

**Removing a connection a Thread uses.** Removing the running default or embedding connection is refused before
writing (`config_invalid`, `written: false`). Change the default in a separate successful operation before removing
its former connection. Other connections can be removed, including ones used by existing Threads. An already
started request finishes with its cloned client; a subsequent request enters the existing `config_invalid`
`recovery_wait` path, without a panic. `thread set --connection Q --model M` explicitly moves the Thread.

**Who changed what.** The operation receipt holds the caller's identity and the result, with no sensitive
value, for its 24-hour retention. `operation get` reads it, and `result.changed_by` repeats the identity. A
`config_changed` diagnostic records the Origin IDs and field names, never values, in the rotating log, for
after the receipt is gone. A change hint isn't sent: `events.wait` reports Threads, and no existing signal
fits a global connection change. TUI readers use the shared registry directly, without an event or file I/O.

**No owner.** `config get` and `config set` fail with `app_not_running`, and the hint names the alternative.
There is no standalone edit path, since it would have no receipt or record. `--config` is refused on both,
because the file is the one the owner loaded. `config init` and `config check` remain standalone.

## Stored API keys and connection test (#269)

Stage A and B of the owner-side connection setup. A client (the Studio first-run screen, the CLI) can store a key,
check a connection and see where each key comes from without a hand-edited TOML file. No new worker, table, event or
queue: the file work runs on `AttachmentService::control_files()` under `config_write`, the catalog request reuses
`HttpModelClient::models`, and the key is read by the lazy credential lookup in `Connection::credentials_with`.

**Storage.** `credentials.toml` sits beside the loaded `config.toml` (`credential_store::path_beside`):

```toml
[connections.proxy]
api_key = "..."
```

It is a separate file so the rule "no key in `config.toml`" stays true and `config.get`'s `content_hash` never covers a
key. The value is **plain text**: treat the file like a password in a dotfile. Unix creates it `0600` and a save
corrects a looser mode (`permissions_tightened` says so). The save is a temporary file in the same directory created `0600` and never loosened (the replaced file's mode is not
copied, unlike `config.toml`), `sync_all`, then the replace used by `config.set` (`rename`; `ReplaceFileW` on Windows with the same interrupted-replace recovery).
On Windows no mode bits exist and Shellboy changes no ACL: the file gets the directory's default permissions (the user
profile's), and an existing file keeps its ACL through `ReplaceFileW`. This is stated as a fact from the API; the
Windows branch has not been run for this change. There is no keychain integration (decided outside this issue).
Unknown keys, a non-table `connections`, a file over 64 KiB or invalid UTF-8 make the file invalid: it is never
rewritten, and the error names a line number only, because the TOML libraries quote source text in their messages and
those messages are dropped.

**Lookup.** Per request, in `HttpModelClient::headers`: the variable named by `api_key_env` if it has a non-blank value,
else the stored key of this connection name, else the old `MissingCredential` error (same text). The file is opened
only when the environment supplies nothing **and** `Config::stored_keys` (shared with every `Connection`) lists the
connection, with `tokio::fs` and a 64 KiB bound, so a new or replaced key applies to the next request with no restart,
while a connection without a stored key adds no file read or scheduling point to its requests. (An extra hop per
request reordered concurrent summary requests in `response_retries::overflow_trim_keeps_every_user_input_of_the_active_turn`
on the macOS runner.) The key goes in the same header as before (`api_key_header` or `Authorization:
Bearer`). A connection without `api_key_env` can use a stored key too, unless `headers` already defines
`Authorization`: the explicit header wins and `credentials.set` refuses (`stored_key_allowed`). `api_key_header`
no longer requires `api_key_env` (D-269): a header-only connection takes a stored key in that header, and a request
with neither source fails with `ConfigError::MissingKey` (kind `config_missing`) rather than sending nothing. The
"defined once" checks (`headers`, `extra_headers`, session header names) apply to the credential header whenever
either setting exists.

**What leaves the owner.** `credentials.set` takes the value, and no operation returns it. `ConnectionTestArgs`,
`CredentialsSetArgs` have hand-written `Debug`; `Connection` carries only the file's path (an inline test key is a masked header of that one value). Errors
are fixed strings and never carry a value, a line or a parser excerpt. The saved request hash of `credentials.set` uses a
digest of the value salted with a per-process random string, so `operation_receipt` holds nothing an offline guess could
confirm; the receipt result is the reply below. The diagnostic `config_changed` records `credentials.<connection>` as the
field name. `HttpModelClient` adds the key to its `secrets` list, so a provider that echoes it in an error body is masked
as for any other credential, and `connection.test` does not return provider text at all. The CLI reads the key from
standard input and refuses a terminal; there is no argument that carries it.

**Key source in `config.get`.** `loaded.credential_source` and `file.credential_source` map each connection name to
`"env"` (the variable has a value in the owner's environment), `"stored"` (an entry exists and is usable) or `"none"`.
It states presence, never validity, value or length. `credentials_file` gives `path`, `present`, `valid` and an
`error` that names no content. `config.get` reads the file each time; the in-memory `Config::stored_keys` (names only,
shared by clones) feeds `config_setup::report` so the setup hints on the UI thread need no file I/O. It starts from the
file at load and follows `credentials.set` / `credentials.clear`; a hand edit of `credentials.toml` is seen by `config.get` at once but used by requests and the hints only after a restart.

**`connection.test`.** Read-only (no receipt), cancellable. It takes either a saved connection name or a candidate
(`base_url`, `api`, `api_key` or `api_key_env`, `api_key_header`, `ssl_verify`). A saved name is resolved from the
configuration **file** (what the next start will load, so a connection just added by `config.set` can be tested),
falling back to the loaded one. It sends the same bounded model-catalog GET as `config check` once, through a fresh
`HttpModelClient`, and nothing else. The reply holds facts: `sent`, `ok`, `http_status` (null only when no response arrived; a 200 with HTML or no `data` keeps its 200 and is `response`),
`failure` (null, or one of `auth`, `url`, `rate_limited`, `server`, `http_status`, `timeout`, `unreachable`,
`credential_missing`, `invalid_configuration`, `response`), `models` (the whole sorted catalogue, which the client already
bounds at 2 MiB and 10,000 ids; `model_count` is its length), and `credential` (`source` env/stored/inline/none,
`api_key_env`, `env_set`). Success means a catalog answered; it never says the key is valid. A 401 is `auth`, a 404 is
`url` (the API root is probably wrong), and a variable that is unset in the owner is `credential_missing` without a
request. `sent` means a request was attempted, not delivered: a refused connection is `sent: true` with `http_status: null`. An
inline key is put in the candidate's headers (`Authorization: Bearer`, or `api_key_header` itself) for that one request and
is kept nowhere; a candidate never reads `credentials.toml`. The call is dropped with its HTTP request when the caller
disconnects or the owner shuts down (`runtime_observation`), so it has no cancel path of its own. The credential read is
part of the response-header wait, so it obeys cancellation and the deadline.

**Live adoption (stage C).** The existing shared `HttpClients` holds one `RwLock<Arc<...>>` generation containing
connection definitions, defaults and clients. A short read lock only clones the generation. Changed definitions build
new clients; unchanged clients retain connection pools, rate gates and usage observations. Existing requests retain
their old client. The bounded control-file closure validates and durably replaces `config.toml`, then adopts the new
generation before releasing the existing `config_write` guard shared with credential writes. A timed-out caller does
not release that guard early. There is no new worker, table, queue, event, migration or dependency.

`config.get.loaded.values` includes live definitions/defaults; `differing_fields` compares those with the file.
`differs` and `loaded.content_hash` still describe startup file bytes (comments included), not restart necessity.
`restart.all_fields_require_restart` is now false. Each `config.set.changes` entry has `restart_required` and
`applied_to_running_owner`; the result-level flags are ORs of the corresponding entry flags, so a mixed live/restart
edit may return both true. No effective changes means both false. Embedding connection changes report restart=true,
applied=false and remain visible as differing fields. Direct file-only `config_edit::apply` has no running owner and
does not hot-apply. Credentials retain their existing next-request lookup and secret-redaction contract.
