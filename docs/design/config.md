# 設定契約と実装照合

採用設計。設定値・検証・パス解決の現行契約を本書に示す。`src/config.rs` と `tests/config.rs` を対応成果物とする。初期設定資料は非同梱。

`Config` は公開TOML全節の型。各節で `deny_unknown_fields` を適用し、未指定は節単位・フィールド単位とも既定値にする。`compaction.concurrency` は公開設定ではないため受理しない。内部の要約同時実行数1は実行制御側の責務。接続なし、modelなし、embeddingなしを有効な既定状態とし、架空の接続を作らない。モデル/effortの固定カタログを設けない。

数値は1024基数のbaselineを採用。正整数/非負整数、file <= request、warning < max、batch <= buffer、retry遅延個数と正値を検証する。UIキーは修飾子Ctrl/Alt/Shift＋文字または端末キー名を受理し、send/newlineの衝突を拒否する。Enter送信、Ctrl+JとShift+Enter改行が既定。

`Config::parse` は純粋な解析・検証。`Config::load` / `load_with_env` / `load_with_overrides` はblocking I/Oを含み、UI開始前または有限file worker上で呼ぶ。明示ファイルのNotFoundはエラー、既定ファイルのNotFoundだけ組込み値に戻す。存在する不正ファイル・読取り失敗を黙って無視しない。`LoadedConfig` は解決したdata-dirと実際の設定ファイル位置（既定ファイル不存在ならNone）を保持。

`PathEnvironment` はOS・環境パスの注入境界。owner用 `resolve_data_dir` はCLI > SHELLBOY_DATA_DIR > TOML > OS、接続CLI用 `resolve_cli_data_dir` はCLI > SHELLBOY_DATA_DIR > OS。接続CLIはTOMLを読まない。CLI/環境相対data-dirは起動cwd、TOML相対data-dirとshell executableは設定ファイル親へ一度絶対化する。OS既定環境パスは絶対パスを必要とし、解決不能時はエラー。存在しない保存先も扱うためcanonicalizeは行わず、directory作成/lockは起動層が行う。Unixホーム、XDG、macOS Application Support、Windows APPDATA/LOCALAPPDATAは [設定ガイド](../guides/configuration.md) のOS別パスを明示実装し、外部ライブラリ既定に置換しない。

`Connection::endpoint` はHTTP(S) API rootからresponses/embeddingsを一度だけ連結。終端API名、URL内資格情報、query、fragmentを拒否し、URLの推測修正を行わない。固定headersのHTTP名/値、case-insensitive重複、Authorizationとapi_key_envの二重定義を検査する。資格情報取得は `credentials` で要求開始直前に遅延実行し、空/不存在ならエラー。接続、shell env、解決済資格情報のDebugは機密値を伏せる。TOMLエラーには入力本文の抜粋を表示しない。

実装判断: env変数名は移植性のあるASCII英数字/underscore（先頭は英字/underscore）へ制限した。追加headerの空値はHTTP上許容し、API keyの空値は拒否する。明示URL内資格情報を禁止する理由は、秘密がURLを通じてログへ現れることを防ぎ環境変数指定へ統一するため。shell executable未指定はNoneとしてOS shell adapterの候補解決へ渡し、起動時に別shellへfallbackしない。これらは初版契約の具体化として採用し、特殊な環境変数名を実proxy運用で必要とした場合に制限を再確認する。

検証: `cargo test --test config` は既定値、全節の未知キー、型違い、数値関係、接続参照、URL/認証矛盾、秘密伏せ、パス優先順位、明示/既定設定ファイルを対象とする。OS注入によるパス試験は各OSでの実機動作検証を代替しない。shell実行、lock、実proxyはこのモジュールの試験範囲外。

## 接続ごとのTLS検証

`connections.<名前>.ssl_verify` はboolで既定true。明示falseの場合だけ、その接続のHTTPS証明書検証を無効化する。HTTP APIへの直接接続では検証対象がない。HTTPS中継プロキシを経由する場合は、そのプロキシ証明書にも同じ設定を適用する。ConnectionのRust DefaultとTOML省略時をともにtrueへ揃え、文字列や数値は拒否する。ownerは起動時の設定を保持するため変更後は再起動する。独立RAGは実行時に読む同じ接続設定を使う。接続CLIからownerのTLS設定を上書きしない。

## 接続ごとのHTTP待機期限

`connections.<名前>.stream_idle_timeout_ms` は非負整数で既定600,000ms。会話・要約のストリーミングResponses要求で、応答header待機と各受信chunkの間の無通信時間を制限する。0は無通信期限を無効化する。受信が続く間の全体時間に上限を置かず、SSE heartbeatも受信として扱う。

`connections.<名前>.request_timeout_ms` は1,000以上の整数で既定60,000ms。embeddingなど非ストリーミングHTTP要求の全体期限に使う。モデル一覧は同設定と20,000msの短い方を適用する。両設定は接続ごとに独立し、Rust DefaultとTOML省略時の値を揃える。負数・文字列・小数・範囲外整数は拒否する。設定変更はowner再起動後の要求へ適用する。期間の加算はHTTP側でchecked操作を行い、OSの時計で表現できない期限をpanicや即時timeoutへ変換しない。

## 接続ごとの会話再試行

`connections.<名前>.response_max_retries` は0以上5以下の整数で既定2。初回要求に加えて行う会話Responses要求の自動再試行回数で、0は無効にする。`connections.<名前>.response_retry_max_delay_ms` は1以上300,000以下の整数で既定30,000ms。指数バックオフと `Retry-After` による1回の待機時間の上限とする。Rust DefaultとTOML省略時を揃え、負数・文字列・小数・範囲外整数は補正せず拒否する。owner再起動後の要求へ適用する。

設定の消費者は会話Driverだけとし、HTTP層の透過再送、要約、embedding、モデル一覧には適用しない。受信前の一時的失敗だけが対象で、本文が空でもSSEやtool引数の断片を受信した要求は自動再試行しない。HTTPエラー本文の受信は生成ストリームの受信と区別する。`stream_idle_timeout_ms` による停滞は自動再試行の対象外で、`request_timeout_ms` の非ストリーム期限も変更しない。停止は再試行待機を取消し、既存attemptへ失敗・再試行を保存する。自動再試行は上流の未処理を保証せず、重複課金の可能性をなくす設定ではない。判定と実行順序は [model HTTP](model-http.md) に従う。

## R3-6: 設定検証の局所化

Connection内のURL/effort/credential名/header検証はConnectionへ、数値・範囲・retry整合・env・path/keyは各sectionへ移す。Configは既定接続とembeddingの参照関係を持つ。first errorを維持するため、数値positive→worker容量→local相互条件→connection内容→section間参照→env/path/keyの順序を明示し、単にsectionごとの一括検証へ並べ替えない。Ui/Shellなどは既存の段階に合わせた小さな検証関数を持つ。

positive指定の共通化は全fieldが正の数値であるRetention/Limits/Io/Workers/Loggingに限定し、既存section!へ小さなall-positive armを加えて宣言順で検証する。Cacheの0許可、Indexing.retries=0と空delay、mixedなUi/Indexingは明示検証のまま保持し、schema DSLや汎用rule registryへ拡張しない。公開TOML/default/未知field拒否/上限/secret非表示は変えない。

## 起動進捗と初期化の取消（P8）

TUIは一つの端末sessionを先に開き、設定/保存先・owner lock・DB版検査/migration・前回中断整理・Control・接続口の準備を非永続stageとして表示する。進捗は最新1値、結果は1件の有限channelで受け取り、UIはtry_recv・短いstage参照と端末eventだけを扱う。設定/cwd/file処理は既存blocking経路、migrationと中断整理は既存DB worker、残る初期化は既存async runtimeで実行する。新Core業務state・DB表・event・workerは足さない。stageは完了率・処理生存の推測ではなく、開始した処理の表示だけである。

設定ファイルのdata_dir（TOML親基準）をCLI > SHELLBOY_DATA_DIR > TOML > OSの優先順位で解決するには、設定の読取り・純検証がlockに先行する。初期提案のlock→設定検査という順序は「解決前に不明なdata-dirをlockする」とは解釈しない。解決済み保存先のlock後だけDB版検査・migration・書込みを行い、設定不正ならDBを作り直さず停止する。接続CLIは従来どおりTOMLを読まない。

初期化中に会話・slash command・貼付入力は受け付けない。Esc/Ctrl+C/Ctrl+Qは終了意図だけを通知し、開始済みblocking処理・migration・中断整理をabortしない。安全な初期化境界まで待ち、準備済みControl・transport・DBを順に閉じ、実際のwriter終了までowner lockを保持する。強制完了や即時lock解放を宣言しない。結果配送と取消が競合しても、UIへ通常運用を渡す前に終了意図を再検査し、必要なら同じ起動画面でcleanup完了を待つ。RPCのCore serveは通常UIへのhandoff後に開始し、準備中に公開されたtransportで要求が届いてもCoreへ受付しない。

失敗は既存のtyped ApiErrorを人向け表示し、Enter/Esc/終了キーで確認後に端末を戻す。初期化失敗後のDB writerは閉じてから結果を返す。端末自体が壊れた場合は端末sessionを終了してから初期化/cleanupの完了を待ち、成功として扱わない。起動画面はUI-onlyかつ一回限りで、会話下書きや管理popupの状態へ混ぜない。

取消単位は既存DataOwner初期化全体とし、途中futureをdropしない。既にこの単位へ入った場合、終了要求後もlock取得・migration・中断整理まで完了しうる。migration済みの変更を巻き戻したとは表示しない。その後のControl開始・接続口公開は次の取消検査で止める。これ以上細かい取消APIやCore状態を増やさず、実writerとlockの寿命を一貫させるための選択である。
