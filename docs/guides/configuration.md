# Configuration reference

[Guide index](README.md) · [User Guide](user-guide.md) · [CLI reference](cli-reference.md)

Configuration is TOML read when the owner (`shellboy` without a subcommand) starts, or when `config check` runs. This reference follows the keys and defaults in `src/config.rs`. Omitted sections and keys receive defaults; unknown keys, wrong types, and invalid combinations are rejected with `config_invalid`. Editing the file does not hot reload a running owner.

## First setup without an owner

Published v0.3.0 predates these setup commands. With that older binary, create the
configuration manually using the minimal example below; do not overwrite an
existing file. The commands apply to a build/release containing this feature.

```sh
shellboy config init
# Edit the config_path printed in the JSON result; choose your API root and model.
# Set the environment variable named by api_key_env in this terminal.
shellboy config check
shellboy
```

These commands also work in PowerShell. `init` creates the complete commented
`config.example.toml` template, without keys, and never overwrites an existing
file or symlink. Both commands work without a running owner. They do not create
an owner database. Use the same `--config PATH` for init, check and startup when
choosing a custom location. Init leaves endpoint/model placeholders for you to
edit. Do not paste credential values into TOML: keep `api_key_env` as a variable
**name**, and set its value in the shell that will launch Shellboy. See the
credential examples later in this guide. Restart an existing owner after edits.

`config check` reads the same strict TOML and reports the resolved paths, missing
connection/model settings and credential-variable presence (never its value).
It sends only a bounded **GET model catalog** request, like `model list`, and
checks whether `default_model` is advertised. It sends no conversation or
embedding request. Success is not proof that a provider supports Responses or
will accept a billed request. A model absent from the catalog may be a supported
proxy alias; confirm that separately rather than guessing. `config check` is an
explicit network operation; the startup welcome performs no network probe.

`base_url` must be the **API root**, for example `https://proxy.example/v1`, not
`https://proxy.example/v1/responses`. Shellboy appends `/models` for catalog checks
and `/responses` for conversations. A 404 check explains this pitfall without
rewriting your URL; also check your proxy's routing. Unknown fields such as the
removed `compact_hint_tokens` are errors, not ignored options.

Precedence: config `--config` > OS default; owner/check data directory
`--data-dir` > `SHELLBOY_DATA_DIR` > TOML `data_dir` > OS default.
Owner-RPC CLI commands still use their separate data-directory rules below.
The empty-conversation welcome shows missing setup facts, the actual selected
configuration path and a minimal example. A fully configured local setup shows
no setup notice; that does not establish network reachability.

### A keyless local Responses-compatible endpoint

If a local server implements **both** the model catalog and Responses API and
requires no authentication, omit `api_key_env` (do not set it to an empty string):

```toml
default_connection = "local"
default_model = "YOUR_LOCAL_MODEL_ID"
[connections.local]
base_url = "http://127.0.0.1:1234/v1"
```

Check the actual server port and advertised model ID. Keyless GET behavior is
verified with a loopback mock, not a particular Ollama/LM Studio release; their
Responses compatibility is not established here. Do not weaken authentication
on a remotely exposed endpoint to copy this local-only example.

## Starting on each platform

The **terminal** displaying the TUI, the **user's shell** launching Shellboy, and the **configured shell** executing model commands are separate choices. You can launch from PowerShell while giving the model Git Bash. Choose the binary matching your OS and CPU from [Releases](https://github.com/tya5/shellboy/releases).

| Environment / purpose | Where to start the TUI | Binary | Model shell configuration |
|---|---|---|---|
| Windows / PowerShell | PowerShell in Windows Terminal or WezTerm, or a PowerShell console | Windows | Default `powershell.exe`; specify `executable` for PowerShell 7 |
| Windows / use Git Bash for the model | Same as above; launching from PowerShell is fine | Windows | Set `kind = "bash"` and Git for Windows `bin\bash.exe` |
| Git Bash window (mintty) | Direct TUI startup depends on the setup and is unverified; first try Windows Terminal or a similar terminal | Windows | Same Windows configuration above; choosing the terminal is a separate step |
| WSL | An interactive terminal inside WSL | Linux | Default Bash; no manual `XDG_RUNTIME_DIR` setting needed |
| macOS / Linux | An interactive terminal on that OS | Matching OS | Default Bash |

For a Git Bash profile in Windows Terminal, use Git for Windows `bin\bash.exe`, rather than `git-bash.exe`, which opens a new mintty window. Configure the terminal profile and the model's `[shell]` settings below separately.

### Windows shell examples

Windows PowerShell is the default. Launching Shellboy from PowerShell 7 does not automatically select pwsh for the model. To select PowerShell 7 explicitly:

```toml
[shell]
kind = "powershell"
executable = 'C:\Program Files\PowerShell\7\pwsh.exe'
```

To give the model Git Bash:

```toml
[shell]
kind = "bash"
executable = 'C:\Program Files\Git\bin\bash.exe'
```

These are alternatives. Do not duplicate `[shell]` in one file: choose one and adjust the path to the actual installation. Prefer `Git\bin\bash.exe`, which prepares MSYSTEM and PATH before starting the internal Bash, over `Git\usr\bin\bash.exe`, which bypasses that environment setup. See the [Git for Windows explanation](https://gitforwindows.org/git-wrapper.html).

Do not use `git-bash.exe` as the model shell executable: it launches a terminal. `C:\Windows\System32\bash.exe` is a WSL launcher and is rejected at shell execution if configured as Git Bash. Write model commands in the selected shell's syntax (see [PowerShell arguments](shell.md#powershell-arguments)).

Example: launch the Windows binary by an absolute path containing spaces from PowerShell:

```powershell
Set-Location C:\work\project
& 'C:\Program Files\Shellboy\shellboy.exe' --config 'C:\config\shellboy.toml'
```

### Distinguishing mintty from WSL

A mintty PTY combined with native Windows console APIs may fail to connect raw mode, input, or screen control correctly. ConPTY support changes this behavior, so this is not a claim that every configuration fails. See the [official mintty documentation](https://mintty.github.io/mintty.1.html).

A user reported failure to start from mintty. The Git for Windows version and ConPTY settings are unknown, and development-side interactive acceptance on real hardware is unperformed. First try PowerShell in Windows Terminal or WezTerm. `winpty shellboy.exe` or a ConPTY configuration may be alternatives, but neither has been verified with Shellboy. `winpty` also requires its own executable.

Inside WSL, install and run the Linux binary. Launching WSL Bash as the model shell from native Windows is outside the supported configuration. Windows interoperability from WSL does not imply that every such combination is adopted by Shellboy. Use Linux-side configuration and storage in WSL.

Starting the Windows `shellboy.exe` from a WSL terminal does not automatically pass environment variables such as API keys defined only inside WSL. Explicit forwarding requires checking [WSLENV](https://learn.microsoft.com/en-us/windows/wsl/filesystems#share-environment-variables-between-windows-and-wsl-with-wslenv) registration and direction flags. A user also reported credentials not being forwarded; use the Linux binary inside WSL.

### Verified scope

A user regularly runs Shellboy from PowerShell with Git Bash as the model shell in a WezTerm + herdr environment. This is a usage report, separate from formal development-side interactive hardware acceptance. Windows native CI covers PowerShell pipe execution, termination, environment variables, and child-process management; it does not substitute for interactive Windows Terminal/WezTerm or real Git Bash/pwsh/mintty acceptance. Those checks and WSL hardware acceptance remain unperformed by the developers. A remaining Windows check is that the short `shellboy` CLI name works after the Git Bash `bin\bash.exe` wrapper starts when the parent PATH omits Shellboy's directory. On macOS, the real binary was launched by absolute path with its directory removed from the parent PATH, and the model's Bash could invoke the CLI by its short name. See Issue #4 in [acceptance records](../validation/acceptance.md) for exact conditions.

## Minimal installation from source

Check the [development prerequisites and OS setup](../development/setup.md) in the environment containing the repository. You need the pinned Rust toolchain (`rust-toolchain.toml`) and a C toolchain to build bundled SQLite and other native dependencies. Keep configuration, data, and credentials outside the source tree.

Bash example:

```bash
cd /path/to/shellboy
cargo build --locked --release --bin shellboy
./target/release/shellboy --config /absolute/path/config.toml
```

The last command starts the TUI in an interactive terminal. Save the minimal connection example below with your settings and set the credential environment variable first. In PowerShell, enter the repository, run the same cargo command, and launch `./target/release/shellboy.exe --config C:/absolute/path/config.toml`. Distinguish these instructions from the setup document's record of actual OS checks.

When starting the model's shell, Shellboy automatically prepends its own executable directory to that shell's PATH. **No separate PATH setup is required just to let the model invoke the CLI**, including when Shellboy itself was launched by absolute path.

To launch Shellboy or use its CLI by the short `shellboy` name from your own terminal, add its directory to your user PATH. From the repository, examples are `export PATH="$PWD/target/release:$PATH"` in Bash and `$env:PATH = "$PWD/target/release;$env:PATH"` in PowerShell. This is unnecessary when calling by absolute path. This PATH change affects that terminal's environment. For a permanent installation, use your OS's user bin directory and stop the running owner before replacing the binary during updates.

## Locations and precedence

Linux/WSL does not require manually setting `XDG_RUNTIME_DIR`. Shellboy creates its Unix local communication directory in an available XDG runtime directory or the OS temporary directory. A CLI in another terminal reads `endpoint.json` from the same data directory; the model shell connects using automatically supplied `SHELLBOY_SOCKET` and instance information. You do not need to guess or set these values. On Unix, both new and existing conversation data directories are restricted to private mode 0700; the specified parent directory's permissions are not changed.

On Linux the communication directory is locked while running to protect it from age-based cleanup by systemd-tmpfiles that respects locks. This does not guarantee protection from XDG runtime removal at final logout, OS restart, administrator deletion, or general macOS cleanup. Shellboy does not change OS settings such as linger. If only the CLI loses connectivity while the owner remains alive, check known operation states and saved results, then shut down the owner normally and restart it. Do not unconditionally resend the same operation under another ID merely because the communication directory disappeared.

| OS | Configuration file | Data directory |
|---|---|---|
| macOS | `$HOME/Library/Application Support/shellboy/config.toml` | `$HOME/Library/Application Support/shellboy/data` |
| Linux | `$XDG_CONFIG_HOME/shellboy/config.toml`, otherwise `$HOME/.config/shellboy/config.toml` | `$XDG_DATA_HOME/shellboy`, otherwise `$HOME/.local/share/shellboy` |
| Windows | `%APPDATA%\shellboy\config.toml` | `%LOCALAPPDATA%\shellboy\data` |

OS environment paths must be absolute. Use `--config PATH` to select a configuration file. A missing default file allows startup with internal defaults, but an unreadable explicitly selected file is an error. Connection and model defaults are empty, so this alone does not allow a new conversation to run.

Owner data-directory precedence is `--data-dir` > `SHELLBOY_DATA_DIR` > TOML `data_dir` > OS default. Relative CLI/environment paths resolve against startup cwd; relative TOML `data_dir` and `shell.executable` paths resolve against the configuration file's parent. TOML strings do not expand `~` or `$HOME` like a shell.

Subcommand connection resolution uses **`--data-dir` > `SHELLBOY_DATA_DIR` > OS default**. The CLI does not read the owner's TOML. `--config` alone cannot connect to an owner using another data directory; supply the same `--data-dir`. Storage is independent of conversation cwd, and only one owner may use a data directory.

## Minimal connection example

This is a generic example. Set the URL, model IDs, and credentials for your Responses-compatible proxy. The example model strings are neither guaranteed executable models nor a built-in catalog.

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
# response_header_timeout_ms = 120000
# stream_idle_timeout_ms = 600000
# request_timeout_ms = 60000
# response_max_retries = 8
# response_retry_max_delay_ms = 60000
# response_retry_partial = true
# auto_trim_on_disconnect = true

# Optional. Specify both fields if enabled.
# [embedding]
# connection = "proxy"
# model = "YOUR_EMBEDDING_MODEL_ID"
```

Set the process environment with `export SHELLBOY_PROXY_API_KEY='…'` in Bash or `$env:SHELLBOY_PROXY_API_KEY = '…'` in PowerShell. Choose a method appropriate to your environment that avoids leaving secrets in history or shared configuration. The binary does not automatically read `credentials.env`.

`base_url` is the HTTP(S) API root; Shellboy appends `/responses` or `/embeddings`. URLs containing username/password, query, fragment, or an already appended endpoint suffix are rejected. `api_key_env` is the **name** of an environment variable; its value is read immediately before sending a request and used as `Authorization: Bearer ...`. Unset, empty, or newline-containing values are errors.

`connections.<name>.headers` is an optional HTTP header dictionary. Names must be unique ignoring case, and invalid control characters in values are rejected. `api_key_env` cannot be combined with an Authorization header. Prefer environment-variable references to writing secrets directly in headers. `default_effort` is an optional nonempty string accepted by the endpoint. There is no built-in catalog of model capacity or effort support.

`ssl_verify` controls HTTPS certificate verification per connection and defaults to `true`. Set `ssl_verify = false` in the same `[connections.proxy]` only when disabling verification for that connection, for example for a self-signed certificate. It applies to conversation, summary, embedding, and model-list requests on the connection, skipping issuer, expiry, and hostname verification. Direct HTTP APIs have no certificate to verify. With an HTTPS forwarding proxy, it also applies to that proxy's certificate. Restart after changing owner settings. Standalone RAG follows the connection configuration read at invocation. No environment-only switch disables verification.

Conversations use Responses; indexing and search queries use the Embeddings API. Omit both `embedding.connection` and `embedding.model`, or specify both. The named connection must exist. Changing embedding settings selects another space and does not silently reindex all historical data. See [history and search](history-and-search.md) for explicit backfill/rebuild and costs. Searches can also incur API usage.

Optional `embedding.batch_max_inputs` defaults to 1 (range 1..128), retaining
singleton requests. `embedding.batch_max_chars` defaults to 64000 Unicode
characters (range 1..1048576); when batching, it must cover
`indexing.split_chars`. Choose caps supported by your endpoint; these settings
do not detect provider limits. Batching reduces requests, not the billed token
volume by definition. A rejected batch records its members and falls back to
individual requests; authentication/configuration failures keep normal pause
rules. The connection's `request_timeout_ms` is one deadline for the entire
batch. When raising `batch_max_inputs`, also review that timeout and the proxy's
accepted input/character caps; a larger batch must still finish within the
configured whole-request deadline. Search query embeddings remain singleton.
Restart the owner after changing settings.

Fetch models with TUI `/models` or `shellboy model list`; change the current Thread model by selecting in `/models` or running `shellboy thread set --model MODEL_ID`. Available models depend on the endpoint's configuration.

The owner reuses HTTP clients per connection. Restart after changing TOML or HTTP(S) proxy/NO_PROXY environment settings. Environment-referenced headers such as API keys are read per request, but exporting a value in another shell does not change the running owner's process environment. Only after a successfully completed SSE response does Shellboy try to reuse the connection by draining up to one second / 64KiB of trailing body. Failure of this post-completion cleanup alone does not return the answer to recovery wait. Cancellation takes priority.

## Per-Thread session identity

With default `connections.<name>.session_header = true`, conversation and summary Responses requests send the Thread UUID in `x-shellboy-session-id`. It remains stable across retries and model changes in the same conversation; child conversations and forks have different Thread UUIDs. The value contains no conversation body, title, or working directory. It is not automatically attached to embedding or model-list requests.

When automatic attachment is enabled, the Thread UUID takes precedence on Responses requests even if `headers` contains the same header with different casing. `session_header = false` disables only automatic attachment: explicitly configured fixed headers are still sent. Other fixed headers and their application to embedding/model-list requests are unchanged.

Setting `prompt_cache_key = true` in the same connection section additionally sends the Thread UUID as `prompt_cache_key` in conversation and summary request bodies. It defaults to `false` and adds nothing to the body. Keep it disabled for providers that do not accept the field. Restart the owner after changing either setting.

These settings provide identifiers the endpoint can use; they do not guarantee cache hits, routing to one backend, or recovery from communication failures. With LiteLLM or similar proxies, check endpoint session-routing settings. A fixed `x-litellm-session-id` or trace ID may take priority over Shellboy's Thread identity in some configurations. Do not combine a shared fixed identity header with Thread identity when conversation isolation is the goal.

F6 details for a completed answer show saved `usage.input_tokens_details.cached_tokens` as `入力キャッシュ` (input cache). Zero and unreported values are distinct. The UI does not infer cache hits from old records or invalid values and also displays the corresponding model request ID.

## HTTP timeouts

Per-connection `response_header_timeout_ms` defaults to 120,000ms (two minutes) and must be positive. It bounds the interval from request start, including upload, until response headers arrive. It does not establish whether the peer received the complete request.

`stream_idle_timeout_ms` defaults to 600,000ms (10 minutes). After response headers arrive, conversation and summary streams are interrupted if this time passes between the last received chunk and the next. Conversations follow retry settings below; summaries wait for a recovery decision. Heartbeats count as received data. A response that keeps receiving data is not stopped simply because total elapsed time exceeds ten minutes. Increase the value for models with long silent periods. `0` disables only this body-idle deadline; the positive header deadline, cancellation and disconnect detection remain active.

`request_timeout_ms` is the total deadline for nonstreaming requests such as model lists and embeddings: default 60,000ms (one minute), minimum 1,000ms. Model lists use the shorter of this setting and 20,000ms, so the default is twenty seconds. Set either in the same `[connections.proxy]`, independently per connection. Restart to apply changes to the owner; standalone RAG uses settings read at invocation. Conversation idle timeouts follow the retry settings below. Summary timeouts do not automatically resend.

## Automatic conversation retries

Per-connection `response_max_retries` defaults to 8 and accepts 0–20. Consecutive temporary HTTP errors, disconnections, EOFs, and receive-idle timeouts trigger at most this many additional attempts. `0` disables them. Consecutive failures reset after a final response is validated, saved, and adopted: a failure early in a long tool loop does not consume the budget for later independent temporary failures. Context-overflow compaction alone does not reset it.

`response_retry_max_delay_ms` caps one exponential-backoff delay: default 60,000ms, range 1–300,000ms. Server `Retry-After` is separate and may wait up to 600,000ms (ten minutes). Stopping cancels the wait. If Retry-After exceeds 600 seconds, Shellboy preserves and displays the requested delay and enters recovery wait instead of shortening it and resending.

`response_retry_partial` defaults to `true`. Temporary disconnection, EOF, or idle timeout retries even after part of the body or tool arguments arrived. Reasoning `response.output_item.added` events, including encrypted reasoning, also count as partial output, so with `false` even an empty text body does not permit automatic resend after such an event. `false` sends partial-output failures to recovery wait. Requests containing only metadata or heartbeats, without output, remain retryable even with `false`. Authentication, configuration, input-format, and explicit provider failures are not automatically resent.

For a small input whose body source is fully consumed, nine silent header waits at the default 120 seconds plus backoff delays of 1+2+4+8+16+32+60+60=183 seconds take up to about 1,263 seconds (21 minutes). If headers arrive but each body then stalls for 600 seconds, the same attempts can take about 5,583 seconds (93 minutes), plus header time. These are examples rather than total deadlines: received data or Retry-After can extend the wait. Large-input early reduction and repeated unconsumed-source failures can end these sequences earlier. Adjust `response_header_timeout_ms`, `stream_idle_timeout_ms` and `response_max_retries` to control each part. Stalled error-body reception on HTTP 429/500/502/503/504 is also retryable.

This applies only to conversations and does not change summary, embedding, or model-list retry policy. Configuration changes apply after owner restart. Duplicate billing cannot be prevented when the upstream processed a request but its connection failed. Failures and retries are recorded in existing attempts, and partially received text is preserved. Tool calls are not executed before adoption of the final response.

## HTTP(S) proxies and exclusions

Separate from the API `base_url`, environment variables can configure an HTTP(S) forwarding proxy. Set them before starting Shellboy and restart to affect a running owner. Standalone RAG also uses its startup environment.

Bash example:

```bash
export HTTP_PROXY=http://proxy.example:8080
export HTTPS_PROXY=http://proxy.example:8080
export NO_PROXY=localhost,127.0.0.1,.internal.example
shellboy
```

PowerShell example:

```powershell
$env:HTTP_PROXY = 'http://proxy.example:8080'
$env:HTTPS_PROXY = 'http://proxy.example:8080'
$env:NO_PROXY = 'localhost,127.0.0.1,.internal.example'
shellboy
```

`HTTP_PROXY` applies to HTTP destinations; `HTTPS_PROXY` to HTTPS destinations. For an ordinary HTTP CONNECT proxy, use `http://...` even as the value of `HTTPS_PROXY`. `ALL_PROXY` supplies a common proxy, overridden by protocol-specific HTTP/HTTPS settings. The library also supports lowercase `http_proxy`, `https_proxy`, `all_proxy`, and `no_proxy`. Do not set upper- and lowercase variants of the same variable to conflicting values.

`NO_PROXY` is a comma-separated list of excluded hosts, IPs, domains, and similar entries. `NO_PROXY='*'` excludes every destination. For direct local API access, specify `localhost,127.0.0.1,::1`. These are process-wide environment settings; there are no per-connection TOML `proxy` / `no_proxy` keys. The current build uses environment settings and does not enable automatic discovery of system proxies configured through macOS/Windows GUIs. SOCKS support is also disabled.

See [reqwest proxy behavior](https://docs.rs/reqwest/0.13.5/reqwest/#proxies) and [NO_PROXY syntax](https://docs.rs/reqwest/0.13.5/reqwest/struct.NoProxy.html#method.from_env).

## Startup settings and Thread settings

`default_connection` / `default_model` apply to new root Threads. Existing Threads store connection/model/effort/cwd. Explicit `shellboy thread create` arguments override inheritance/defaults; children inherit their parent's settings. `thread set` changes a target: an in-flight HTTP result uses the settings at request start, and new settings apply at the next safe boundary. Model changes advance the reasoning epoch, preventing resend of the old model's reasoning items. Changing effort alone does not revive old reasoning.

`--effort ''` explicitly clears effort; omission inherits it. TOML `default_effort = ""` is rejected. When passing an empty string from PowerShell, check that the native argument-passing mode preserves it.

A root Thread defaults to `AGENTS.md` in its creation cwd; children/forks inherit the parent's current reference. Use `thread create/set/fork --agents PATH` to explicitly choose another reference. Relative paths resolve against the target's effective cwd, including a simultaneous cwd change. Changing cwd alone never rediscovers AGENTS. Explicit paths must be nonempty, existing readable regular UTF-8 files of at most 8 MiB: missing is `invalid_argument`, access/UTF-8 failures are `io_failed`, oversize is `input_limit`. There is no clearing operation or ancestor scan. Selection validation does not pin the file. Each new Turn reads current bytes and saves a DB snapshot; explicit retry of the same Turn reuses its snapshot even after path changes. If the selected file is later deleted, the next Turn retains the existing missing-means-no-instructions behavior. Later unreadable, invalid UTF-8 or oversized content remains a preparation error awaiting recovery. `thread get` exposes `agents_path`; reference changes record the actor, old/new paths and time in `context_changed` and notify the next new Turn, not the current Turn's later requests/retries. TUI exposes create/fork/settings form fields and `/agents [PATH]` (no argument displays the reference).

## All sections and defaults

Byte values are integers. KiB/MiB/GiB below mean 1024/1048576/1073741824 bytes; TOML does not accept `32 MiB`. Duration suffixes `_days` and `_ms` mean days and milliseconds.

| Section | Key = default | Purpose |
|---|---|---|
| root | `data_dir` = OS default, `default_connection` / `default_model` = unset, `connections` = empty | Storage and new-conversation defaults |
| `connections.<name>` | `ssl_verify=true`; URL/authentication/effort/headers described above | Per-connection TLS verification, enabled by default |
| `connections.<name>` | `session_header=true`, `prompt_cache_key=false` | Thread identity header and optional cache identity body field for conversations/summaries |
| `connections.<name>` | `response_header_timeout_ms=120000`, `stream_idle_timeout_ms=600000`, `request_timeout_ms=60000` | Positive Responses header deadline; post-header idle (0 disables); nonstream total deadline (minimum 1000ms) |
| `connections.<name>` | `auto_trim_on_disconnect=true` | API-free maximum reduction from the second eligible consecutive failure; no preflight |
| `connections.<name>` | `response_max_retries=8`, `response_retry_max_delay_ms=60000`, `response_retry_partial=true` | Consecutive temporary conversation failure retries (0–20, 0 disables), backoff cap (1–300000ms), and retry after partial reception |
| `embedding` | `connection` / `model` = unset; `batch_max_inputs=1`, `batch_max_chars=64000` | Indexing and search API; bounded synchronous history batches |
| `shell` | `kind` = Unix `bash` / Windows `powershell`, `executable` = automatic, `env` = empty dictionary | Shell kind, executable, additional environment; not direct WSL startup |
| `retention` | `tool_raw_days=7`, `tool_raw_bytes=1073741824`, `clipboard_days=7`, `clipboard_bytes=268435456` | Raw output seven days/1GiB; managed images seven days/256MiB. Active pins/selections/drafts protected |
| `limits` | `attachment_file_bytes=20971520`, `attachment_request_bytes=41943040` | 20MiB per attachment, 40MiB per request |
| `limits` | `shell_raw_bytes=67108864`, `tool_excerpt_bytes=16384` | Shell raw 64MiB, result excerpt 16KiB; history body pages have a fixed 64KiB maximum, not a configuration key |
| `limits` | `shell_stdin_bytes=1048576`, `draft_bytes=1048576` | Stdin and draft, 1MiB each |
| `limits` | `revisit_max_per_run` omitted | Unlimited Revisit consumptions per Run; `0` disables registration, positive integers cap consumptions. Text has a fixed 8192-byte UTF-8 limit |
| `limits` | `data_warning_bytes=8589934592`, `data_max_bytes=10737418240` | Logical usage warning at 8GiB, stop new work at 10GiB |
| `cache` | `body_bytes=33554432`, `layout_bytes=16777216`, `history_bytes=33554432`, `attachment_base64_bytes=67108864` | Body 32MiB, layout 16MiB, send history 32MiB, attachment conversion 64MiB |
| `io` | `file_prepare_timeout_ms=30000`, `shell_write_timeout_ms=10000` | File preparation and stdin-write deadlines |
| `io` | `term_grace_ms=2000`, `drain_timeout_ms=1000`, `self_reply_grace_ms=250`, `shutdown_grace_ms=5000` | Termination grace, pipe drain, self-stop reply, shutdown UI grace |
| `io` | `output_buffer_bytes=8388608`, `output_batch_bytes=65536`, `output_batch_ms=25` | Raw collection buffer 8MiB, DB batch 64KiB/25ms |
| `workers` | `attachment=2`, `control_file=1` | Bounded file-worker counts |
| `indexing` | `concurrency=1`, `split_chars=2000`, `retries=2`, `common_failure_pause_count=3`, `retry_delays_ms=[1000,4000]` | Embedding concurrency, chunk characters, retries, common-failure pause |
| `ui` | `max_fps=30`, `paste_fold_chars=1000`, `paste_fold_lines=3`, `send_key="Enter"`, `newline_keys=["Ctrl+J","Shift+Enter"]`, `copy_backend="auto"`, `palette="dark"`, `bell=true` | Rendering, paste folding, send/newline, copy method, header/user surfaces, terminal bell on recovery wait |
| `logging` | `file_bytes=5242880`, `files=3` | Diagnostic logs, 5MiB ×3 |

`cache.history_bytes` exclusively budgets the cross-Thread send-history conversion LRU. `body_bytes` retains UI bodies and node metadata; `layout_bytes` budgets render layouts including widths/folds; `attachment_base64_bytes` budgets converted attachments. These are not a whole-process RSS cap including active viewport/HTTP pins, SQLite/OS caches, runtime resources, or saved history. Cache eviction does not delete history; data is fetched again as needed. A zero cache budget disables retained caching, not all active working memory.

Other numeric settings generally require positive integers. Exceptions include all four `cache.*` byte budgets (`body_bytes`, `layout_bytes`, `history_bytes`, `attachment_base64_bytes`), `connections.<name>.stream_idle_timeout_ms=0`, `connections.<name>.response_max_retries=0`, `indexing.retries=0`, and matching `retry_delays_ms=[]`. A zero cache budget disables retained caching; it does not select the default budget. These exceptions do not permit zero for `io.*` or the indexing chunk size `indexing.split_chars`. Retry delays must contain as many positive integers as retries. `wait --timeout-ms 0` is immediate CLI observation, not a configuration setting. Worker counts also have runtime-supported upper bounds.

Additional constraints: attachment_file ≤ attachment_request, data_warning < data_max, output_batch ≤ output_buffer. `shell.env` maps valid environment names to values without NUL. `copy_backend` is `auto|os|osc52`. Send/newline keys combine supported names such as Enter with Ctrl/Alt/Shift; the newline list cannot be empty or overlap the send key. `shell.kind` accepts only `bash|powershell`.

`ui.palette` accepts `dark` (default) or `light` for the header and message bands.
Nonempty `NO_COLOR` uses plain markers and spacing. Terminals without advertised
256-color/truecolor support use reverse/separator fallback. Capability detection
happens once at UI startup and does not probe the terminal background.

Raw/managed retention is separate from conversation retention. Conversations, reasoning, and summaries cannot be configured for automatic expiry deletion. See [recovery and storage](recovery-and-storage.md) for capacity limits, explicit deletion, and GC.

## Implementation references

Keys, defaults, validation, and OS paths are checked against [config.rs](../../src/config.rs), stored Thread settings against [DB operations](../../src/control/db.rs), AGENTS reading against [instructions.rs](../../src/instructions.rs), and send boundaries against [driver.rs](../../src/control/driver.rs). See the [configuration design](../design/config.md) for the contract.

### Failure-driven reduction

`auto_trim_on_disconnect` defaults to true and enables maximum safe omission
from the second eligible consecutive failure; false leaves ordinary retries.
`response_max_retries` (default eight) bounds every automatic resend including
a reduced request. `compact_hint_tokens` is removed and rejected as an unknown
field; remove it from configuration before restarting. There is no pre-POST
activation or token estimate. See [failure-driven reduction](recovery-and-storage.md#failure-driven-reduction-50).
