# 設定リファレンス

[English](configuration.md)

[ガイド入口](README.ja.md) · [User Guide](user-guide.ja.md) · [CLI辞書](cli-reference.ja.md)

設定はowner（サブコマンドなしの `shellboy`）起動時に読むTOMLです。現行 `src/config.rs` のキーと既定値に対応します。省略した節・キーには既定値が入り、未知キー、型違い、不正な組合せは `config_invalid` で拒否します。ファイルを書き換えても稼働ownerへhot reloadされません。

## 環境別の起動方法

TUIを表示する**端末**、Shellboyを起動する**利用者のshell**、モデルのコマンドを実行する**設定内のshell**は別のものです。PowerShellから起動してモデルにはGit Bashを使わせる構成もできます。配布binaryは [Releases](https://github.com/tya5/shellboy/releases) からOS・CPUに合うものを選びます。

| 環境・用途 | TUIを起動する場所 | 使うbinary | モデルのshell設定 |
|---|---|---|---|
| Windows / PowerShell | Windows TerminalやWezTermのPowerShell、又はPowerShellのコンソール | Windows版 | 既定の`powershell.exe`。PowerShell 7は`executable`を指定 |
| Windows / Git Bashを使わせる | 同上。起動元はPowerShellでもよい | Windows版 | `kind = "bash"`とGit for Windowsの`bin\bash.exe`を指定 |
| Git Bashの窓（mintty） | 直接のTUI起動は構成依存・未検証。まずWindows Terminalなどへ切り替える | Windows版 | 上のWindows用設定と同じ。端末を変える操作とは別 |
| WSL | WSLの中の対話端末 | Linux版 | 既定のBash。`XDG_RUNTIME_DIR`の手動設定は不要 |
| macOS / Linux | 各OSの対話端末 | 各OS版 | 既定のBash |

Windows TerminalのプロファイルにGit Bashを設定して起動する場合も、minttyを新しく開く`git-bash.exe`ではなくGit for Windowsの`bin\bash.exe`を使います。端末起動用プロファイルと、以下のモデル用`[shell]`設定は別々に指定します。

### Windowsのshell設定例

既定はWindows PowerShellです。起動元がPowerShell 7でも、モデル用shellを自動的にpwshへ切り替えることはありません。PowerShell 7を明示する例:

```toml
[shell]
kind = "powershell"
executable = 'C:\Program Files\PowerShell\7\pwsh.exe'
```

Git Bashをモデル用shellにする例:

```toml
[shell]
kind = "bash"
executable = 'C:\Program Files\Git\bin\bash.exe'
```

この2例は選択肢です。同じ設定ファイルに`[shell]`を重複させず、利用する方だけを選び、インストール先が違えば実在するパスへ変えます。`Git\bin\bash.exe`はMSYSTEMやPATHを整えて内部のBashを起動するため、環境準備を省く`Git\usr\bin\bash.exe`の直接指定よりこちらを使います。[Git for Windowsの説明](https://gitforwindows.org/git-wrapper.html)も参照してください。

`git-bash.exe`は端末を開くランチャーなので、モデル用shellの実行ファイルには使いません。`C:\Windows\System32\bash.exe`はWSLランチャーで、Git Bashとして使おうとするとshell実行時に拒否されます。モデルのcommandは選んだshellの構文で書きます（[shellガイド](shell.ja.md#powershellの引数)）。

PowerShellから、空白を含むパスに置いたWindows版を絶対パスで起動する例:

```powershell
Set-Location C:\work\project
& 'C:\Program Files\Shellboy\shellboy.exe' --config 'C:\config\shellboy.toml'
```

### minttyとWSLの区別

minttyのPTYとネイティブWindowsのコンソールAPIの組合せでは、raw modeや入力・画面制御がうまくつながらないことがあります。ただしConPTY対応で状況が変わるため、すべての構成で起動不可能とは断定しません。[minttyの公式説明](https://mintty.github.io/mintty.1.html)を参照してください。

minttyからは利用者の環境で起動できなかった報告があります。Git for Windowsの版とConPTY設定は未特定で、開発側の対話実機受入は未実施です。問題時はまずWindows TerminalやWezTermのPowerShellで起動してください。minttyでの`winpty shellboy.exe`やConPTY設定は代替候補ですが、Shellboyでの動作確認はできていません。`winpty`を使う場合も別途その実行ファイルが必要です。

WSLでは、そのLinux環境内にLinux版を配置して起動します。Windows本体からWSLのBashをモデル用shellとして起動する構成は対象外です。WSLからWindowsプログラムを呼べることと、Shellboyで採用している構成は区別してください。WSLの保存先・設定はLinux側のものを使います。

WSLの端末からWindows版の`shellboy.exe`を起動すると、WSL側だけで設定したAPIキーなどの環境変数は自動では渡りません。明示的な受け渡しには[WSLENV](https://learn.microsoft.com/en-us/windows/wsl/filesystems#share-environment-variables-between-windows-and-wsl-with-wslenv)の登録と方向指定の確認が必要です。利用者からも資格情報が渡らなかった報告があるため、WSL内ではLinux版を使ってください。

### 確認済みの範囲

Windowsでは、PowerShellから起動し、モデル用shellにGit Bashを設定した構成が利用者のWezTerm + herdr環境で日常的に使われています。これは利用報告であり、開発側の正式な対話実機受入とは区別します。Windows native CIでPowerShellのpipe実行・終了・環境変数・子プロセス管理などを確認していますが、Windows Terminal/WezTermの対話操作やGit Bash/pwsh/minttyの実機受入を代替するものではありません。開発側ではこれらとWSL実機の確認は未実施です。Windowsでの残る確認には、親PATHにShellboyの配置先がない条件で、Git Bashの`bin\bash.exe` wrapper起動後も短い名前の`shellboy`でCLIを呼べることを含みます。macOSでは実binaryを絶対パスで起動し、親PATHからその配置先を外した状態でも、モデルのBashから短い名前でCLIを実行できることを確認しました。詳しい条件は [検証状況](../validation/acceptance.md) のIssue #4を参照してください。

## ソースからの最小導入

リポジトリを配置した環境で、[開発環境の前提とOS別準備](../development/setup.md) を確認してください。固定Rust toolchain（`rust-toolchain.toml`）と、bundled SQLite等をビルドするCツールチェーンが必要です。設定・データ・資格情報はソースツリーへ入れません。

Bash例：

```bash
cd /path/to/shellboy
cargo build --locked --release --bin shellboy
./target/release/shellboy --config /absolute/path/config.toml
```

最後のコマンドは対話端末からTUIを起動します。後述の最小接続例を自分の環境へ合わせて保存し、資格情報の環境変数を先に設定してください。PowerShellではリポジトリへ移動して同じcargoコマンドを実行し、`./target/release/shellboy.exe --config C:/absolute/path/config.toml` で起動します。OS別の実機確認範囲はsetupの検証記録と区別してください。

モデルのshellを起動するとき、Shellboyは自分の実行ファイルがあるディレクトリを、そのshellのPATHの先頭へ自動で追加します。**モデルからCLIを呼ぶためだけのPATH設定は不要**です。Shellboy自体を絶対パスで起動した場合も同じです。

利用者が自分の端末から `shellboy` という短い名前で起動・CLI操作したい場合は、配置先を利用者側のPATHへ登録してください。Bashならリポジトリ内から `export PATH="$PWD/target/release:$PATH"`、PowerShellなら `$env:PATH = "$PWD/target/release;$env:PATH"` のように設定できます。絶対パスを指定して呼ぶ場合はこの登録も不要です。PATH追加はその端末の環境です。恒久配置する場合は自分のOSのユーザー用binを使い、更新時は稼働ownerを終了してから置き換えます。

## 配置と優先順位

Linux/WSLでも `XDG_RUNTIME_DIR` の手動設定は不要です。Unixのローカル通信領域はShellboyが利用可能なXDG runtime directoryかOSの一時領域に用意します。別端末のCLIは同じdata-dirの `endpoint.json` を読み、モデルのshellは自動設定された `SHELLBOY_SOCKET` とinstance情報で接続します。これらの値を自分で推測・設定する必要はありません。Unixの会話保存先は新規・既存とも自分専用の0700へ限定し、指定した親ディレクトリの権限は変更しません。

Linuxでは通信directoryを動作中lockし、lockを尊重するsystemd-tmpfilesの古さによる清掃から保護します。
最終logoutによるXDG runtime領域の削除、OS再起動、管理者による削除やmacOSの清掃全般を防ぐ保証はありません。
Shellboyはlinger等のOS設定を変更しません。ownerが動作中なのにCLIだけ接続できなくなった場合は、
既知operationの状態と保存済み結果を確認し、ownerを通常終了して起動し直してください。
通信領域が消えても、同じ操作を別IDで無条件に再送しないでください。


| OS | 設定ファイル | データディレクトリ |
|---|---|---|
| macOS | `$HOME/Library/Application Support/shellboy/config.toml` | `$HOME/Library/Application Support/shellboy/data` |
| Linux | `$XDG_CONFIG_HOME/shellboy/config.toml`、未設定なら `$HOME/.config/shellboy/config.toml` | `$XDG_DATA_HOME/shellboy`、未設定なら `$HOME/.local/share/shellboy` |
| Windows | `%APPDATA%\shellboy\config.toml` | `%LOCALAPPDATA%\shellboy\data` |

OS環境パスは絶対パスが必要です。`--config PATH` で設定ファイルを指定できます。既定ファイルが存在しない場合は内部既定で起動できますが、明示したファイルが読めなければエラーです。接続とモデルの既定値は空なので、未設定のまま新規会話を実行できるわけではありません。

ownerのデータ保存先は `--data-dir` > `SHELLBOY_DATA_DIR` > TOMLの `data_dir` > OS既定の順です。CLI/環境変数の相対パスは起動cwd基準、TOMLの `data_dir` と `shell.executable` の相対パスは設定ファイルの親ディレクトリ基準です。TOML文字列の `~` や `$HOME` をshellのように展開する機能はありません。

サブコマンドの接続先解決は **`--data-dir` > `SHELLBOY_DATA_DIR` > OS既定** です。CLIはownerのTOMLを読みません。`--config` だけでは別データ保存先のownerへ接続できないため、同じ `--data-dir` を指定してください。保存先は会話のcwdと独立し、同じ保存先のownerは一つです。

## 最小の接続例

以下は汎用の設定例です。URL・モデルID・資格情報は利用するResponses互換proxyに合わせて設定してください。例のモデル文字列は実行保証や組込みモデル一覧を意味しません。

```toml
# config.toml
# data_dir = "data"
default_connection = "proxy"
default_model = "YOUR_RESPONSES_MODEL_ID"

[connections.proxy]
base_url = "https://proxy.example/v1"
api_key_env = "SHELLBOY_PROXY_API_KEY"
# default_effort = "low"
# ssl_verify = true
# session_header = true
# prompt_cache_key = false
# stream_idle_timeout_ms = 600000
# request_timeout_ms = 60000
# response_max_retries = 8
# response_retry_max_delay_ms = 60000
# response_retry_partial = true
# auto_trim_on_disconnect = true
# compact_hint_tokens = 75000

# 任意。設定する場合は二つとも指定する。
# [embedding]
# connection = "proxy"
# model = "YOUR_EMBEDDING_MODEL_ID"
```

Bashでは `export SHELLBOY_PROXY_API_KEY='…'`、PowerShellでは `$env:SHELLBOY_PROXY_API_KEY = '…'` のようにプロセス環境へ設定します。秘密値を履歴や共有設定へ残さない方法は各環境で選んでください。binary自体は `credentials.env` を自動読込みしません。

`base_url` はHTTP(S)のAPIルートです。末尾の `/responses`・`/embeddings` はshellboyが付加します。URL内のユーザー名/パスワード、query、fragment、既に付いたendpoint末尾は拒否します。`api_key_env` は環境変数の**名前**で、要求送信直前に値を読み `Authorization: Bearer ...` にします。未設定/空の値や改行入り値はエラーです。

`connections.<名前>.headers` は任意のHTTP header辞書です。header名は大文字小文字を無視して重複不可、値の不正な制御文字は拒否します。`api_key_env` とAuthorization headerは併用不可です。headerへ秘密値を直接書くより環境変数参照を使ってください。`default_effort` は省略可能な非空文字列で、接続先が受け付ける値を指定します。モデル容量・effort対応のカタログは内蔵しません。

`ssl_verify` は接続ごとのHTTPS証明書検証で、既定は `true` です。自己署名証明書などを使う接続で無効化する場合だけ、同じ `[connections.proxy]` に `ssl_verify = false` を指定します。その接続の会話・要約・埋め込み・モデル一覧に適用され、証明書の発行元・期限・ホスト名の検証が省略されます。HTTP APIへ直接接続する場合は検証対象がありません。HTTPSの中継プロキシを使う場合は、そのプロキシ証明書の検証にも適用されます。owner設定の変更後は再起動してください。独立RAGは実行時に読む接続設定へ従います。環境変数だけで検証を無効化する機能はありません。

会話はResponses、索引と検索クエリはEmbeddings APIを使います。`embedding.connection` と `embedding.model` は両方省略、または両方指定です。接続名は定義済みである必要があります。embedding変更時は別空間として扱い、過去データ全件を無断で再索引化しません。明示backfill/rebuildと費用の考え方は [履歴と検索](history-and-search.ja.md) を参照してください。検索にもAPI利用が生じます。

モデル一覧はTUIの `/models` または `shellboy model list` で取得し、現在のThreadのモデルは `/models` で選択するか `shellboy thread set --model MODEL_ID` で変更できます。利用可能なモデルは接続先の設定に依存します。

ownerは接続ごとのHTTPクライアントを再利用します。TOMLやHTTP(S) proxy/NO_PROXYの環境設定を変えた場合はownerを再起動してください。APIキーなどの環境変数参照headerは要求ごとに読みますが、別のshellでexportしても既存ownerのプロセス環境は変わりません。正常完了したSSEだけ、最大1秒・64KiBの後続本文読捨てで接続再利用を試みます。完了後の後処理の失敗だけで回答を復旧待ちへ戻しません。取消しは優先します。

## Thread単位のセッション識別

既定の `connections.<名前>.session_header = true` では、会話と要約のResponses要求に `x-shellboy-session-id` headerを送り、Thread UUIDを渡します。同じ会話では再試行やモデル切替後も同じ値を使い、子会話やforkは別のThread UUIDになります。会話本文・タイトル・作業場所などは値へ含めません。embeddingとモデル一覧には自動付与しません。

自動付与が有効な場合、`headers` に大文字小文字違いを含む同名headerを指定していても、Responses要求ではThread UUIDを優先します。`session_header = false` は自動付与だけを無効にし、明示した固定headersはそのまま送ります。ほかの固定headerや、embedding・モデル一覧への固定headersの適用は変わりません。

`prompt_cache_key = true` を同じ接続節へ指定すると、会話と要約の要求本文にもThread UUIDを `prompt_cache_key` として送ります。既定は `false` で本文へ追加しません。この項目を受け付けないproviderでは無効のまま使ってください。両設定とも変更後はownerを再起動します。

これらは接続先が使える識別情報であり、キャッシュhit・同一backendへの振り分け・通信障害の解消を保証しません。LiteLLM等を使う場合は接続先のsession routing設定も確認してください。`x-litellm-session-id` やtrace IDが優先される構成では、そちらへ固定値を指定するとShellboyのThread識別より優先される可能性があります。会話ごとの分離が目的なら、全会話共通の固定識別headerを併用しないでください。

完成した回答のF6詳細では、保存されたモデル要求の `usage.input_tokens_details.cached_tokens` を「入力キャッシュ」として確認できます。数値0と未報告は区別します。古い記録や不正な値からキャッシュhitを推測せず、対応するモデル要求IDも表示します。

## HTTPの待機時間

接続ごとの `stream_idle_timeout_ms` は既定600,000ms（10分）です。会話・要約のストリームで応答headerが届くまで、または最後の受信chunkから次のchunkまで、この時間を超えると要求を中断します。会話は以下の再試行設定に従い、要約は復旧判断を待ちます。heartbeatも受信として数えます。応答を継続して受信している間は、10分を超えても全体の経過時間では中断しません。長時間無通信になるモデルには値を増やせます。`0` はこの無通信期限を無効にします。取消し操作と通信切断の検知は引き続き有効です。

`request_timeout_ms` はモデル一覧・embeddingなどの非ストリーミング要求の全体期限で、既定60,000ms（1分）、最小1,000msです。モデル一覧はこの設定と20,000msの短い方を使うため、既定では20秒です。両設定は同じ `[connections.proxy]` に指定し、接続ごとに変更できます。ownerへ反映するには再起動します。独立RAGは実行時に読む接続設定を使います。会話の無通信期限は以下の再試行設定に従います。要約の待機期限では自動再送しません。

## 会話の自動再試行

接続ごとの `response_max_retries` は既定8、指定範囲0〜20です。一時的なHTTPエラー・接続切断・EOF・受信停滞timeoutが連続した場合、この回数まで追加試行します。`0` で無効にできます。最終responseが検証・保存され採用された後は連続失敗回数がリセットされるため、長いtoolループの初期に失敗があっても、後の独立した一時失敗には再び予算を使えます。context超過の圧縮だけではリセットしません。

`response_retry_max_delay_ms` は指数バックオフの1回の待機上限で、既定60,000ms、指定範囲1〜300,000msです。サーバーからの `Retry-After` はこの設定と別で最大600,000ms（10分）まで待ちます。停止すると待機も取消します。 Retry-Afterが600秒（10分）を超える場合、600秒に短縮して再送せず、指定待機時間を保存・表示して復旧待ちにします。

`response_retry_partial` は既定 `true` です。本文やtool引数を一部受信した後でも、一時的な通信切断・EOF・無通信期限なら再試行します。推論の項目（暗号化された推論を含む）の `response.output_item.added` も部分出力として数えるため、本文がまだ空でも `false` では自動再送しません。`false` にすると、部分出力がある失敗は自動再送せず復旧待ちにします。metadataやheartbeatだけで出力がない要求は `false` でも再試行対象です。認証・設定・入力形式・明示provider失敗などは自動再送しません。

無通信のまま毎回600秒で失敗し、Retry-Afterの指定がない例では、最初の要求と8回の追加試行が600秒×9、待機が1+2+4+8+16+32+60+60=183秒で、復旧待ちまで約5,583秒（93分）かかります。全体期限ではなく、受信やRetry-Afterによってさらに長くなることもあります。短くしたい場合は `stream_idle_timeout_ms` と `response_max_retries` を調整してください。HTTP 429/500/502/503/504のエラー本文の受信が停滞した場合も再試行対象です。

対象は会話だけで、要約・embedding・モデル一覧の再試行方針は変更しません。設定変更はowner再起動後に反映されます。上流が処理済みなのに接続だけ失敗した場合の重複課金までは防げません。失敗と再試行は既存attemptへ記録し、部分受信の本文も保存します。最終responseが採用される前のtool callは実行しません。

## HTTP(S)プロキシと除外先

APIの `base_url` とは別に、HTTP(S)通信の中継プロキシを環境変数で指定できます。Shellboy起動前に設定し、稼働ownerへ反映するには再起動します。独立RAGにも起動時の環境が適用されます。

Bash例：

```bash
export HTTP_PROXY=http://proxy.example:8080
export HTTPS_PROXY=http://proxy.example:8080
export NO_PROXY=localhost,127.0.0.1,.internal.example
shellboy
```

PowerShell例：

```powershell
$env:HTTP_PROXY = 'http://proxy.example:8080'
$env:HTTPS_PROXY = 'http://proxy.example:8080'
$env:NO_PROXY = 'localhost,127.0.0.1,.internal.example'
shellboy
```

`HTTP_PROXY` はHTTP宛、`HTTPS_PROXY` はHTTPS宛です。通常のHTTP CONNECTプロキシなら、`HTTPS_PROXY` の値も `http://...` を使います。`ALL_PROXY` は両方の共通指定で、個別のHTTP/HTTPS指定が優先します。小文字の `http_proxy`、`https_proxy`、`all_proxy`、`no_proxy` もライブラリが扱います。同名の大文字/小文字を異なる値で併記しないでください。

`NO_PROXY` はカンマ区切りの除外ホスト/IP/ドメインなどを指定します。`NO_PROXY='*'` は全宛先をプロキシ対象外にします。ローカルAPIを直接使う場合は `localhost,127.0.0.1,::1` を指定してください。これらはプロセス全体の環境設定で、接続別TOMLの `proxy` / `no_proxy` キーはありません。現在のビルドは環境変数による指定を利用し、macOS/WindowsのGUIに設定されたシステムプロキシを自動取得するfeatureは有効にしていません。SOCKS対応も有効にしていません。

詳細は [reqwestのproxy仕様](https://docs.rs/reqwest/0.13.5/reqwest/#proxies) と [NO_PROXYの形式](https://docs.rs/reqwest/0.13.5/reqwest/struct.NoProxy.html#method.from_env) を参照してください。

## 起動設定とThread設定

`default_connection` / `default_model` は新しいルートThreadの既定です。既存Threadにはconnection/model/effort/cwdが保存されます。`shellboy thread create` の明示指定が継承/既定より優先し、子は親の設定を継承します。`thread set` で対象を変更でき、進行中HTTP要求の結果は開始時の設定で扱い、次の安全境界で新設定を使います。モデル切替でreasoning epochを進め、古いモデルの推論itemを再送しません。effortだけの変更は過去推論の復活を意味しません。

`--effort ''` は明示解除、引数省略は継承です。TOMLの `default_effort = ""` は拒否されます。PowerShellから空文字を渡す場合は、利用中のPowerShellのnative引数渡し方式で保持されることを確認してください。

ルートThreadのAGENTS参照先は作成cwdの `AGENTS.md` に固定し、子/forkはその参照を継承します。cwdを変えてもAGENTS参照先を探し直しません。各新Turnの準備でUTF-8原文を読みDBへsnapshotを保存します。同じTurnの明示retryはそのsnapshotを再利用し、途中編集を混ぜません。AGENTS不在は指示なし、読めない/不正UTF-8/8MiB超過は準備エラーとして復旧判断を待ちます。祖先ディレクトリを走査する設定やAGENTS pathを変更する公開flagはありません。

## 全設定節と既定値

バイト値は整数です。下表のKiB/MiB/GiBはそれぞれ1024/1048576/1073741824バイトを表し、TOMLに `32 MiB` と書くことはできません。期間の `_days` は日、`_ms` はミリ秒です。

| 節 | キー = 既定値 | 用途 |
|---|---|---|
| root | `data_dir` = OS既定、`default_connection` / `default_model` = 未設定、`connections` = 空 | 保存先・新規会話の既定 |
| `connections.<名前>` | `ssl_verify=true`、接続URL/認証/effort/headerは上記 | 接続単位のTLS検証。既定で検証あり |
| `connections.<名前>` | `session_header=true`, `prompt_cache_key=false` | 会話・要約のThread識別headerと任意のキャッシュ識別本文 |
| `connections.<名前>` | `stream_idle_timeout_ms=600000`, `request_timeout_ms=60000` | ストリームの無通信期限（0で無効）・非ストリームの全体期限（最小1000ms） |
| `connections.<名前>` | `auto_trim_on_disconnect=true`, `compact_hint_tokens` 未指定 | 大きい入力で連続再試行を消費した後、一度だけ決定論的縮小を検討 |
| `connections.<名前>` | `response_max_retries=8`, `response_retry_max_delay_ms=60000`, `response_retry_partial=true` | 会話の連続一時失敗の再試行（0〜20回、0で無効）・指数待機上限（1〜300000ms）・部分受信後も再試行 |
| `embedding` | `connection` / `model` = 未設定 | 索引・検索API |
| `shell` | `kind` = Unix `bash` / Windows `powershell`、`executable` = 自動、`env` = 空辞書 | shell種別・実行ファイル・追加環境。WSL直接起動の指定ではない |
| `retention` | `tool_raw_days=7`, `tool_raw_bytes=1073741824`, `clipboard_days=7`, `clipboard_bytes=268435456` | raw出力7日/1GiB、managed画像7日/256MiB。使用中pin/選択/下書きは保護 |
| `limits` | `attachment_file_bytes=20971520`, `attachment_request_bytes=41943040` | 添付1件20MiB・要求40MiB |
| `limits` | `shell_raw_bytes=67108864`, `tool_excerpt_bytes=16384`, `history_read_bytes=65536` | shell raw64MiB・結果抜粋16KiB・本文ページ64KiB |
| `limits` | `shell_stdin_bytes=1048576`, `draft_bytes=1048576` | stdin・下書き各1MiB |
| `limits` | `data_warning_bytes=8589934592`, `data_max_bytes=10737418240` | 論理使用量の警告8GiB・新規作業停止10GiB |
| `cache` | `body_bytes=33554432`, `layout_bytes=16777216`, `history_bytes=33554432`, `attachment_base64_bytes=67108864` | 本文32MiB・layout16MiB・送信履歴32MiB・添付変換64MiB |
| `io` | `file_prepare_timeout_ms=30000`, `shell_write_timeout_ms=10000` | ファイル準備・stdin書込みの待機期限 |
| `io` | `term_grace_ms=2000`, `drain_timeout_ms=1000`, `self_reply_grace_ms=250`, `shutdown_grace_ms=5000` | 停止猶予・pipe回収・自停止返信・終了UI猶予 |
| `io` | `output_buffer_bytes=8388608`, `output_batch_bytes=65536`, `output_batch_ms=25` | raw収集buffer8MiB・DB batch64KiB/25ms |
| `workers` | `attachment=2`, `control_file=1` | 有限ファイルworker数 |
| `indexing` | `concurrency=1`, `split_chars=2000`, `retries=2`, `common_failure_pause_count=3`, `retry_delays_ms=[1000,4000]` | embedding並列数・分割文字数・再試行・共通障害pause |
| `ui` | `max_fps=30`, `paste_fold_chars=1000`, `paste_fold_lines=3`, `send_key="Enter"`, `newline_keys=["Ctrl+J","Shift+Enter"]`, `copy_backend="auto"`, `bell=true` | 描画・貼付け折畳み・送信/改行・copy方式・復旧待ちの端末ベル |
| `logging` | `file_bytes=5242880`, `files=3` | 診断log5MiB×3 |

`cache.history_bytes` は全Thread共有の送信履歴変換LRU専用です。`body_bytes` はUI本文とnode metadataの保持、`layout_bytes` は幅・折畳み等を含む描画layout LRU、`attachment_base64_bytes` は添付変換結果です。active viewportやHTTP送信中のpin等の作業領域、SQLite/OS cache、runtime資源、保存履歴を含む全プロセスRSSの上限ではありません。cache退避は履歴削除ではなく必要時に再取得します。各cache予算0は保持cacheを無効化します。使用中の作業領域までゼロになる意味ではありません。

それ以外の数値は原則正整数です。例外は `connections.<名前>.stream_idle_timeout_ms=0`、`connections.<名前>.response_max_retries=0`、`indexing.retries=0` と対応する `retry_delays_ms=[]`。retry_delaysはretriesと同数の正整数が必要です。`wait --timeout-ms 0` は設定ではなくCLIの即時観測です。worker数にはruntimeが扱える上限もあります。

追加制約は attachment_file ≤ attachment_request、data_warning < data_max、output_batch ≤ output_bufferです。`shell.env` は有効な環境変数名とNULを含まない値の辞書。`copy_backend` は `auto|os|osc52`。送信/改行キーはEnter等の対応キー名とCtrl/Alt/Shiftの組合せで、改行キー一覧は空不可、送信キーとの重複不可です。`shell.kind` は `bash|powershell` のみです。

raw/managed保持期限と会話保持は別です。会話・推論・要約には自動期限削除を設定できません。容量限界、明示削除、GCの扱いは [復旧と保存](recovery-and-storage.ja.md) を参照してください。

## 照合先

キー・既定・検証・OSパスは [config.rs](../../src/config.rs)、Threadへの設定保存は [DB操作](../../src/control/db.rs)、AGENTS読取りは [instructions.rs](../../src/instructions.rs)、送信境界は [driver.rs](../../src/control/driver.rs) と照合しています。設定の契約は [設定設計](../design/config.md) を参照してください。


### 接続断後の縮小設定

`connections.<名前>.auto_trim_on_disconnect` は既定trueです。会話の連続再試行を使い切った大きい入力に対し、Turn/generationごと一度だけAPI不要のspill/省略を検討します。`false`で無効です。`compact_hint_tokens` は省略可能な正の整数で、指定時は起動目安と縮小目標です。0、負数、小数、文字列は拒否します。未指定時は推定75,000 tokens超で検討し、その時点の推定の半分を目標にします。context上限の設定ではありません。owner再起動後に適用されます。

推定は保存された直近成功usageと入力文字数から求め、利用できない場合は4文字/tokenを使います。送信前に正確なtokensは分からず、添付・多言語・provider差があります。挙動と復元は[接続断後の復旧](recovery-and-storage.ja.md#接続断の再試行後に入力を縮める)を参照してください。
