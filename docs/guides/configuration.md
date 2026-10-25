# 設定リファレンス

[ガイド入口](README.md) · [User Guide](user-guide.md) · [CLI辞書](cli-reference.md)

設定はowner（サブコマンドなしの `shellboy`）起動時に読むTOMLです。現行 `src/config.rs` のキーと既定値に対応します。省略した節・キーには既定値が入り、未知キー、型違い、不正な組合せは `config_invalid` で拒否します。ファイルを書き換えても稼働ownerへhot reloadされません。

## ソースからの最小導入

リポジトリを配置した環境で、[開発環境の前提とOS別準備](../development/setup.md) を確認してください。固定Rust toolchain（`rust-toolchain.toml`）と、bundled SQLite等をビルドするCツールチェーンが必要です。設定・データ・資格情報はソースツリーへ入れません。

Bash例：

```bash
cd /path/to/shellboy
cargo build --locked --release --bin shellboy
./target/release/shellboy --config /absolute/path/config.toml
```

最後のコマンドは対話端末からTUIを起動します。上の最小接続例を自分の環境へ合わせて保存し、資格情報の環境変数を先に設定してください。PowerShellではリポジトリへ移動して同じcargoコマンドを実行し、`./target/release/shellboy.exe --config C:/absolute/path/config.toml` で起動します。OS別の実機確認範囲はsetupの検証記録と区別してください。

モデルのshellからも `shellboy` CLIを呼ぶため、生成binaryをPATH上のディレクトリへ配置するか、起動前にそのディレクトリをPATHへ追加します。例えばBashでリポジトリ内から `export PATH="$PWD/target/release:$PATH"`、PowerShellで `$env:PATH = "$PWD/target/release;$env:PATH"` とします。PATH追加はその端末の環境です。恒久配置する場合は自分のOSのユーザー用binを使い、更新時は稼働ownerを終了してから置き換えます。

## 配置と優先順位

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

# 任意。設定する場合は二つとも指定する。
# [embedding]
# connection = "proxy"
# model = "YOUR_EMBEDDING_MODEL_ID"
```

Bashでは `export SHELLBOY_PROXY_API_KEY='…'`、PowerShellでは `$env:SHELLBOY_PROXY_API_KEY = '…'` のようにプロセス環境へ設定します。秘密値を履歴や共有設定へ残さない方法は各環境で選んでください。binary自体は `credentials.env` を自動読込みしません。

`base_url` はHTTP(S)のAPIルートです。末尾の `/responses`・`/embeddings` はshellboyが付加します。URL内のユーザー名/パスワード、query、fragment、既に付いたendpoint末尾は拒否します。`api_key_env` は環境変数の**名前**で、接続開始直前に値を読み `Authorization: Bearer ...` にします。未設定/空の値や改行入り値はエラーです。

`connections.<名前>.headers` は任意のHTTP header辞書です。header名は大文字小文字を無視して重複不可、値の不正な制御文字は拒否します。`api_key_env` とAuthorization headerは併用不可です。headerへ秘密値を直接書くより環境変数参照を使ってください。`default_effort` は省略可能な非空文字列で、接続先が受け付ける値を指定します。モデル容量・effort対応のカタログは内蔵しません。

会話はResponses、索引と検索クエリはEmbeddings APIを使います。`embedding.connection` と `embedding.model` は両方省略、または両方指定です。接続名は定義済みである必要があります。embedding変更時は別空間として扱い、過去データ全件を無断で再索引化しません。明示backfill/rebuildと費用の考え方は [履歴と検索](history-and-search.md) を参照してください。検索にもAPI利用が生じます。

モデル一覧はTUIの `/models` または `shellboy model list` で取得し、現在のThreadのモデルは `/models` で選択するか `shellboy thread set --model MODEL_ID` で変更できます。利用可能なモデルは接続先の設定に依存します。

## 起動設定とThread設定

`default_connection` / `default_model` は新しいルートThreadの既定です。既存Threadにはconnection/model/effort/cwdが保存されます。`shellboy thread create` の明示指定が継承/既定より優先し、子は親の設定を継承します。`thread set` で対象を変更でき、進行中HTTP要求の結果は開始時の設定で扱い、次の安全境界で新設定を使います。モデル切替でreasoning epochを進め、古いモデルの推論itemを再送しません。effortだけの変更は過去推論の復活を意味しません。

`--effort ''` は明示解除、引数省略は継承です。TOMLの `default_effort = ""` は拒否されます。PowerShellから空文字を渡す場合は、利用中のPowerShellのnative引数渡し方式で保持されることを確認してください。

ルートThreadのAGENTS参照先は作成cwdの `AGENTS.md` に固定し、子/forkはその参照を継承します。cwdを変えてもAGENTS参照先を探し直しません。各新Turnの準備でUTF-8原文を読みDBへsnapshotを保存します。同じTurnの明示retryはそのsnapshotを再利用し、途中編集を混ぜません。AGENTS不在は指示なし、読めない/不正UTF-8/8MiB超過は準備エラーとして復旧判断を待ちます。祖先ディレクトリを走査する設定やAGENTS pathを変更する公開flagはありません。

## 全設定節と既定値

バイト値は整数です。下表のKiB/MiB/GiBはそれぞれ1024/1048576/1073741824バイトを表し、TOMLに `32 MiB` と書くことはできません。期間の `_days` は日、`_ms` はミリ秒です。

| 節 | キー = 既定値 | 用途 |
|---|---|---|
| root | `data_dir` = OS既定、`default_connection` / `default_model` = 未設定、`connections` = 空 | 保存先・新規会話の既定 |
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
| `ui` | `max_fps=30`, `paste_fold_chars=1000`, `paste_fold_lines=3`, `send_key="Enter"`, `newline_keys=["Ctrl+J","Shift+Enter"]`, `copy_backend="auto"` | 描画・貼付け折畳み・送信/改行・copy方式 |
| `logging` | `file_bytes=5242880`, `files=3` | 診断log5MiB×3 |

`cache.history_bytes` は全Thread共有の送信履歴変換LRU専用です。`body_bytes` はUI本文とnode metadataの保持、`layout_bytes` は幅・折畳み等を含む描画layout LRU、`attachment_base64_bytes` は添付変換結果です。active viewportやHTTP送信中のpin等の作業領域、SQLite/OS cache、runtime資源、保存履歴を含む全プロセスRSSの上限ではありません。cache退避は履歴削除ではなく必要時に再取得します。各cache予算0は保持cacheを無効化します。使用中の作業領域までゼロになる意味ではありません。

それ以外の数値は原則正整数です。例外は `indexing.retries=0` と対応する `retry_delays_ms=[]`。retry_delaysはretriesと同数の正整数が必要です。`wait --timeout-ms 0` は設定ではなくCLIの即時観測です。worker数にはruntimeが扱える上限もあります。

追加制約は attachment_file ≤ attachment_request、data_warning < data_max、output_batch ≤ output_bufferです。`shell.env` は有効な環境変数名とNULを含まない値の辞書。`copy_backend` は `auto|os|osc52`。送信/改行キーはEnter等の対応キー名とCtrl/Alt/Shiftの組合せで、改行キー一覧は空不可、送信キーとの重複不可です。`shell.kind` は `bash|powershell` のみです。

raw/managed保持期限と会話保持は別です。会話・推論・要約には自動期限削除を設定できません。容量限界、明示削除、GCの扱いは [復旧と保存](recovery-and-storage.md) を参照してください。

## 照合先

キー・既定・検証・OSパスは [config.rs](../../src/config.rs)、Threadへの設定保存は [DB操作](../../src/control/db.rs)、AGENTS読取りは [instructions.rs](../../src/instructions.rs)、送信境界は [driver.rs](../../src/control/driver.rs) と照合しています。設定の契約は [設定設計](../design/config.md) を参照してください。
