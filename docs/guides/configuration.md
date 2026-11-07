# Configuration reference

[Guide index](README.md) · [User Guide](user-guide.md) · [CLI reference](cli-reference.md)

Configuration is TOML read when the owner (`shellboy` without a subcommand) starts, or when `config check` runs. This reference follows the keys and defaults in `src/config.rs`. Omitted sections and keys receive defaults; unknown keys, wrong types, and invalid combinations are rejected with `config_invalid`. Editing the file does not hot reload a running owner.

## Migrating terminal width configuration

Builds containing the terminal-input redesign remove `ui.ambiguous_width`, the
manual one/two-column override for East Asian Ambiguous characters introduced
in v0.7.0. Remove only `ambiguous_width = ...` from your existing `[ui]` section;
keep its other settings. There is no replacement option. Leaving this key in
place causes the strict parser to reject the **whole configuration**, rather
than ignoring it. Restart using the same configuration path after editing.

Terminal capability measurement now starts without blocking input or the first
frame. A cached successful observation is an initial baseline, not a user
override; late observations can update rendering. Missing or inconsistent
measurements remain unknown and use conservative fallback glyphs and widths.
Guessed measurements are never saved. This change is included in v0.8.0.

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
embedding request. Success is not proof that a provider supports the selected conversation API or
will accept a billed request. A model absent from the catalog produces a warning but check still succeeds
(decision D-009): it may be a supported proxy alias; confirm that separately rather than guessing. `config check` is an
explicit network operation; the startup welcome performs no network probe.

`base_url` must be the **API root**, for example `https://proxy.example/v1`, not
`https://proxy.example/v1/responses`. Shellboy appends `/models` for catalog checks
and `/responses` (default) or `/chat/completions` for conversations. A 404 check explains this pitfall without
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
# session_header_names = ["X-Shellboy-Session-Id", "x-litellm-session-id"]
# prompt_cache_key = false
# prompt_cache_diagnostics = false
# prompt_caching = true
# response_header_timeout_ms = 120000
# stream_idle_timeout_ms = 600000
# request_timeout_ms = 60000
# response_max_retries = 8
# response_retry_max_delay_ms = 60000
# response_retry_partial = true
# auto_trim_on_disconnect = true
# auto_at_tokens = 200000   # overrides [compaction] auto_at_tokens; 0 disables
# segment_chars = 64000     # overrides [compaction] segment_chars (2000..1000000)
# reduce_rounds = 4         # overrides [compaction] reduce_rounds (1..16)

# Optional. Specify both fields if enabled.
# [embedding]
# connection = "proxy"
# model = "YOUR_EMBEDDING_MODEL_ID"
```

Set the process environment with `export SHELLBOY_PROXY_API_KEY='…'` in Bash or `$env:SHELLBOY_PROXY_API_KEY = '…'` in PowerShell. Choose a method appropriate to your environment that avoids leaving secrets in history or shared configuration. The binary does not automatically read `credentials.env`.

`base_url` is the HTTP(S) API root; Shellboy appends the selected conversation path (`/responses`, `/chat/completions`, or `/messages`), `/models`, or `/embeddings`. URLs containing username/password, query, fragment, or an already appended endpoint suffix are rejected. `api_key_env` is the **name** of an environment variable; its value is read immediately before sending a request. By default it is sent as `Authorization: Bearer ...`. Optional `api_key_header` sends that value **unchanged**, without a Bearer prefix, in the named header instead (for example `x-api-key`, Azure's `api-key`, or a gateway-specific name). This setting works for every dialect and requires `api_key_env`. Unset, empty, or newline-containing credential values are errors. Header names must be valid HTTP tokens, at most 256 bytes, and cannot be transport-owned names.

`connections.<name>.headers` is an optional HTTP header dictionary. Names must be unique ignoring case, and invalid control characters in values are rejected. The header selected by `api_key_env`/`api_key_header` cannot also appear in `headers` or `extra_headers`, even under different capitalization. Prefer environment-variable references to writing secrets directly in headers. `default_effort` is an optional nonempty string accepted by the endpoint. There is no built-in catalog of model capacity or effort support.

### Anthropic Messages: `api = "anthropic_messages"`

Use this to talk to Anthropic's own Messages API (`POST {base_url}/messages`),
for example with an Anthropic Console API key (the way a Claude plan's API credit
is used), which exposes native prompt caching and signed thinking. Through a LiteLLM
or similar gateway, the gateway's own dialect (`responses`/`chat_completions`) is
what Shellboy sees; this setting is for the native API.

```toml
default_connection = "claude"
default_model = "YOUR_CLAUDE_MODEL_ID"

[connections.claude]
api = "anthropic_messages"
base_url = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY" # Set this environment variable before starting Shellboy.
api_key_header = "x-api-key"     # Raw key value, not Authorization: Bearer.
# max_tokens = 32768
# prompt_caching = true   # Default: Shellboy places the cache markers itself.
# To enable thinking: extra_body = { thinking = { type = "adaptive" } }
# extra_headers = { "anthropic-beta" = "..." }
```

**Prompt caching is on by default.** Without cache markers every request's full
input is billed at the full rate, which for long agent conversations can cost
several times more. Shellboy knows where the unchanging start of a request ends
and where the growing conversation ends, so it places the markers itself: one
`cache_control: {type: "ephemeral"}` (the default 5-minute cache) on the last
tool, one on the system prompt, and one on the last block of the conversation,
before any trailing Shellboy notices (event notices and the saved context line).
Within a Turn each request only appends to the previous one, so the earlier
entries stay readable. That is three of the four markers the API
allows. `prompt_caching = false` on the connection sends no marker at all.
`cache_control` is reserved: setting it in `extra_body` is a configuration error.
Cache-read behavior still needs the approved live verification described below;
synthetic fixtures are not evidence of a real provider cache hit.

- Shellboy sends the minimum a conversation needs: `model`, `max_tokens`
  (required by the API), `stream`, `messages`, a top-level `system` and `tools`,
  and the fixed header `anthropic-version: 2023-06-01`, plus the cache markers
  above unless `prompt_caching = false`. **Nothing else optional is sent except
  the Thread effort**, which goes as `output_config.effort` when one is set
  (`minimal` is sent as `low`; other values unchanged) and is omitted otherwise.
  `output_config` and `cache_control` are reserved, so `extra_body` cannot set them.
  Thinking, temperature, `tool_choice` and beta headers are yours to add with
  `extra_body` and `extra_headers`. Thinking on/off is not tied to effort:
  `output_config.effort` guides how much the model works, while `thinking` is a
  separate setting, and some models accept only certain combinations or reject
  one of them with HTTP 400. Check the provider's documentation for the model.
  Newer models reject some
  parameters (for example `temperature`) with HTTP 400; Shellboy never sends them.
- `max_tokens` (positive integer, only valid with this dialect) defaults to 32768,
  below the 128K output cap documented for current models. Raise it for very long
  answers; a turn that reaches it is reported as incomplete, not as a success.
- Authentication: prefer `api_key_env = "ANTHROPIC_API_KEY"` with
  `api_key_header = "x-api-key"`, as in the example. No key is pasted into TOML.
  Omitting `api_key_header` retains the default Bearer path; whether that path
  authenticates a real Anthropic key has not been verified here.
- Context Shellboy inserts mid-conversation (event notices) is sent as a user
  message (the API has no mid-conversation system role), so `context_role` has no
  effect here. A request must end with a user message; Shellboy refuses to send an
  assistant prefill (current models reject it) and reports `input_invalid`.
- Thinking, `redacted_thinking` and block types Shellboy does not know are stored
  unread and sent back **unchanged** in the same dialect, which the API requires
  for signed thinking. They are never shown as answer text, never searched or
  summarized, and never sent to a different dialect or after the Thread's
  connection or model changes. Key order inside such a block is not preserved;
  its text and signature are byte-exact.
- Stop reasons: `end_turn`, `stop_sequence` and `tool_use` finish normally.
  `max_tokens` and `model_context_window_exceeded` are incomplete. `refusal`,
  `pause_turn` and any unknown value fail with the original response kept;
  Shellboy does not guess a continuation.
- Usage: total input is `input_tokens` + `cache_creation_input_tokens` +
  `cache_read_input_tokens`; the cached count is `cache_read_input_tokens`; the
  cache-write count `cache_creation_input_tokens` is saved as
  `input_tokens_details.cache_creation_tokens`; output tokens come from the last
  (cumulative) report. Missing counters stay unknown.
  Look at the saved usage to see whether the second request read from the cache.
- Errors keep the provider's error type as the provider code. HTTP 429, 529 and
  5xx use the existing retry rules; 400 and 413 are not retried. A spend-limit 429
  has no retry hint and keeps failing until access resumes.
- Fixture tests check Shellboy's side of the documented wire format. Real-provider
  behavior (cache reads, thinking round trips, rate limits) is verified separately
  and only with your approval.

### Native Messages Verification Still Pending

No real provider call is part of the synthetic fixture qualification. After API
credit returns, explicit user approval is required before verifying: a second
request has positive `cache_read_input_tokens`; cache-creation facts can be
recorded for that observation; authentication works with the chosen Bearer or
`x-api-key` path; and thinking/signature blocks survive a provider round trip.
Shellboy currently maps cache reads into its standard cached counter but has no
separate saved `cache_creation` counter. Whether to persist one is a separate
issue; this change adds no storage column and does not claim that check passed.


### Context role: `context_role`

Shellboy inserts context into a conversation: event notices (`Shellboy event v1: ...`, for example a finished background shell or a changed context) and a context line saved at each Turn start and after a setting change. The context line is stored as a `context` history node and reused by following requests; a changed setting appends a new line rather than rewriting earlier history. These items are sent as `developer` messages by default, the meaning OpenAI-style APIs expect.

Some gateways (for example LiteLLM in front of Claude) move every `developer` message into the system prompt. A conversation whose last message is then an assistant message is rejected as "assistant message prefill" (HTTP 400, `The conversation must end with a user message`), which happens when a notice is the only thing after the model's last answer. For such a connection set:

```toml
[connections.claude]
context_role = "user"
```

With `context_role = "user"`, every `developer` message after the leading system message is sent with `role: user` and unchanged text, so the `Shellboy ... v1:` marker still tells the model it is a notification rather than something you typed. The default is `"developer"`. Only these two values are accepted. The mapping is applied when a request is sent, not when history is saved, so changing it later affects only new requests. Restart the owner after changing it.

### Provider-specific options: `extra_body` and `extra_headers`

Shellboy sends only what a conversation needs. Everything optional and provider-specific (reasoning settings, sampling, cache hints, beta headers) is yours to add per connection, and **Shellboy does not interpret it**. Providers change what they accept from one model generation to the next, so the list stays in your configuration, not in Shellboy.

```toml
[connections.proxy]
extra_body = { temperature = 0.2, thinking = { type = "enabled", budget_tokens = 2048 } }
extra_headers = { "X-Beta-Feature" = "alpha" }
```

- `extra_body` is a TOML table. Its keys are added to the top level of each conversation and summary request body. It never replaces a field Shellboy sends.
- `extra_headers` is a table of strings added to each conversation and summary request. It is not sent to model-list or embedding requests.
- Both apply to every dialect (`responses`, `chat_completions`, and later ones). With neither set, the request is exactly what it was before.
- Shellboy checks only three things: the value is representable as JSON (no TOML dates or times), it is small (`extra_body` at most 64 KiB serialized, `extra_headers` at most 32 entries of at most 8 KiB each), and it does not collide with a field Shellboy owns. Anything else is passed through and judged by the provider. A rejected value reports `config_invalid` at `shellboy config check` and at startup.
- Reserved `extra_body` top-level keys (rejected, not overridden). `responses`: `model`, `input`, `tools`, `stream`, `store`, `include`, `reasoning`, `previous_response_id`, `prompt_cache_key`. `chat_completions`: `model`, `messages`, `tools`, `stream`, `stream_options`, `reasoning_effort`. `anthropic_messages`: `model`, `max_tokens`, `stream`, `messages`, `system`, `tools`, `output_config`, `cache_control`. A name reserved for one dialect is ordinary for the other.
- `extra_headers` rejects credential-like and transport-owned names (`Authorization`, `Proxy-Authorization`, `Cookie`, `Host`, `Content-Type`, `Content-Length`, `Accept`, `X-Shellboy-Session-Id`, and any name containing `api-key`, `apikey`, `secret`, `token` or `credential`). Use `api_key_env` or `headers` for credentials. A name may not repeat an entry of `headers`.
- Effort: `default_effort` (and a Thread's effort) is sent as `reasoning` (Responses), `reasoning_effort` (Chat) or `output_config.effort` (Messages). Those keys are reserved, so `extra_body` cannot also set them; there is nothing to prioritize. Provider-specific reasoning shapes (for example `thinking`) go in `extra_body`.
- Each model attempt records only a SHA-256 hash of the extra fields it sent (`extra_digest` in its input manifest), so you can later tell whether two attempts used the same settings. The contents stay in your configuration file; they are not copied into the database. Treat `extra_body` values as non-secret: use `api_key_env` for keys.
- Restart the owner after changing either setting. The provider's own documentation, not Shellboy, says which values are valid; add and remove entries as the provider changes.

`ssl_verify` controls HTTPS certificate verification per connection and defaults to `true`. Set `ssl_verify = false` in the same `[connections.proxy]` only when disabling verification for that connection, for example for a self-signed certificate. It applies to conversation, summary, embedding, and model-list requests on the connection, skipping issuer, expiry, and hostname verification. Direct HTTP APIs have no certificate to verify. With an HTTPS forwarding proxy, it also applies to that proxy's certificate. Restart after changing owner settings. Standalone RAG follows the connection configuration read at invocation. No environment-only switch disables verification.

Conversations use the selected API dialect; indexing and search queries keep using the Embeddings API. Omit both `embedding.connection` and `embedding.model`, or specify both. The named connection must exist. Changing embedding settings selects another space and does not silently reindex all historical data. See [history and search](history-and-search.md) for explicit backfill/rebuild and costs. Searches can also incur API usage.

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

## Conversation API dialect

Each connection selects `api = "responses"` (the default),
`api = "chat_completions"` or `api = "anthropic_messages"`. Use an API **root**,
not its final endpoint: Shellboy appends `/responses`, `/chat/completions` or
`/messages` for conversations and summaries.
Catalog (`/models`) and embedding (`/embeddings`) paths are unchanged. Changing
the dialect requires an owner restart. No automatic protocol fallback is tried.

```toml
default_connection = "gemini"
default_model = "YOUR_GEMINI_MODEL_ID"
[connections.gemini]
api = "chat_completions"
base_url = "https://generativelanguage.googleapis.com/v1beta/openai"
api_key_env = "GEMINI_API_KEY"
```

This is an **unverified real-provider example**, based on the documented beta
OpenAI-compatible endpoint, not a tested Gemini connection. No real provider or
paid request was used for this feature. Check current provider documentation and
set a model ID you can access. `config check` only GETs a catalog; it cannot prove
conversation compatibility or actual caching. `shellboy model list --connection
gemini` (owner required) and `config check` expose `api` and
`unavailable_features` in their JSON details. These are format-level limits, not
a provider's advertised model capabilities.

Chat Completions sends the complete available history as `messages`, including
instructions (developer instructions map to system), images, grouped shell tool
calls and their results. It reads one streaming choice, concatenates argument
fragments by tool index, and waits for `finish_reason` plus `[DONE]` before
adopting a successful answer/tool group. Missing terminal frames, invalid calls,
length/filter/unknown completion reasons are not silently successful. Ordinary
transport/cancellation/deadline/retry/reduction/recovery rules stay shared with
Responses; only explicit context-limit codes trigger that classification.

D-010 tolerances keep audit facts rather than quietly rewriting the response:
complete tool calls can end with `stop` (the original value is kept), repeated
full `shell` names are counted and ignored, and absent tool ids are derived
stably from response id plus index with a synthesis marker. If both response id
and tool id are missing, the adapter refuses to guess. Usage chunks may omit
`choices`; an omission counter is retained. These facts live in the saved
output's `chat_completions` metadata and direct-client raw JSON. Incomplete
arguments, missing names and unrelated malformed chunks are still rejected.

Capacity classification uses recognized **string `error.code` values only**;
Gemini numeric-code/`INVALID_ARGUMENT` and OpenRouter-shaped errors without such
a code may reach recovery rather than immediately shrinking input (no guessing).

Responses reasoning items and encrypted reasoning are **not replayed** to Chat.
Vendor `reasoning_content` is intentionally unused (not saved, displayed or
replayed), and vendor-specific opaque tool metadata is not supported. Modes
requiring such replay are outside this first version. `reasoning_effort` is sent
when an effort is configured, but provider acceptance/meaning is unverified.
`prompt_cache_key=true` is Responses-only and is rejected with Chat; omit it.
Provider-specific optional request fields are not built in; add them yourself with `extra_body` and `extra_headers` (next section).

Chat requests `stream_options.include_usage`. Reported `prompt_tokens`,
`completion_tokens`, cached-input and reasoning-token details map to the normal
saved usage fields. Missing values stay unknown; no total or cache hit is
estimated. Inspect `shellboy usage get --thread THREAD_ID` and F6's saved usage
and attempt details for actual reported cached tokens versus unknown counts.
Fixture counts verify the mapping, **not** real-provider cache effectiveness;
provider caching must be qualified separately with explicit paid-test approval.

Anthropic's OpenAI-compatible facade is **not a supported target**: Anthropic
states it is mainly for comparison/testing, lacks prompt caching there and does
not faithfully support the relevant reasoning controls. Long conversations
without caching can be much slower/more expensive. Use `anthropic_messages`
(next section) for the native Anthropic API. Native Gemini APIs are not added. References: [Gemini OpenAI compatibility](https://ai.google.dev/gemini-api/docs/openai)
and [Anthropic compatibility limits](https://platform.claude.com/docs/en/api/openai-sdk).

## Per-Thread session identity

With default `connections.<name>.session_header = true`, conversation and summary requests in either dialect send the Thread UUID in `x-shellboy-session-id`. It remains stable across retries and model changes in the same conversation; child conversations and forks have different Thread UUIDs. The value contains no conversation body, title, or working directory. It is not automatically attached to embedding or model-list requests.

`connections.<name>.session_header_names` is a list of header names; the default is `["X-Shellboy-Session-Id", "x-litellm-session-id"]`. With `session_header = true`, the same Thread UUID is sent under every listed name on the same requests (conversation and summary). Providers ignore headers they do not know, so the extra name is harmless elsewhere. Override the list to change or drop names; an empty list (`[]`) sends nothing, like `session_header = false`.

The default includes `x-litellm-session-id` because LiteLLM reads only that header (or `x-litellm-trace-id`) as a session. Without it, LiteLLM creates a new session for every request, and a ChatGPT backend behind it then routes each request separately so prompt caching does not apply. With the default list, LiteLLM-proxied ChatGPT caching works with no configuration. Shellboy has no LiteLLM-specific code; the name is only a default value.

Measured on 2026-11-01 through an unmodified LiteLLM to a ChatGPT backend (tool loop, six requests of 7,000 to 7,900 tokens): with a fixed session, every request after the first hit the cache almost entirely (6,912 to 7,680 tokens cached); with a new session per request, all six had 0 cached tokens. Sending `x-litellm-session-id` with the Thread UUID gave 97 to 98% cached from the first request, against 35 to 59% worker cache rates when only the unread Shellboy name was sent. Without caching, token consumption rises substantially. `prompt_cache_key` and `x-codex-turn-state` made no difference.

Each name follows the `extra_headers` rules: valid header name (at most 256 bytes, at most 8 names), no credential-like or transport-owned name, no case-insensitive duplicate inside the list. While `session_header = true`, a name also must not repeat a `headers`, `extra_headers` or credential header (for example, a static `x-litellm-session-id` in `headers` is rejected: override the list or set `session_header = false`). `X-Shellboy-Session-Id` in `headers` stays allowed and is overridden by the Thread UUID. `config check` reports violations as `config_invalid`.

## Owned work rows

`ui.live_work_rows` controls how many oldest owned-work rows can be read and
shown above the composer. It defaults to 3 and accepts integers from 1 to 10;
other values are configuration errors. Small terminals may show fewer rows and
report the hidden count. Restart the owner after changing the setting.

## Held Input Rows

`ui.held_input_rows` independently controls the fixed held-input body below live
work and above the composer. It defaults to 3 and accepts integers from 1 to 10;
zero, out-of-range values and other types are configuration errors. Empty bodies
are blank-padded, with a persistent `None` heading. Small terminals can reduce
the body and show a hidden count. Alt+W or heading clicks fold each region for
this UI process only; fold state is not saved to the configuration or database.

## Motion

`ui.motion = "on"` (default) lets at most one thing move in each screen region:
the newest running foreground shell row (or, without one, the streaming answer
row) shows a one-column spinner, the `◉ 抱えている作業` heading mark changes
brightness at most once per second, and the status bar keeps its shimmer/pulse.
Set `"off"` for screen readers or slow links; every region then shows its static
mark (`⏵`, `▸`, `◉`, `●`) with the same text. `NO_COLOR` never blinks.

## Terminal program status

`ui.program_status = "on"` (default) emits generic OSC7501 rev0.4 program status
on the TUI terminal. Set it to `"off"` and restart to emit **no** OSC7501,
including no query or exit clear. No receiver/product detection or external
agent-report command is used. The CLI does not emit TUI program-status reports.

```toml
[ui]
program_status = "off"
```

The root report summarizes all saved conversations, with priority blocked > error
> working > done > idle. At most 32 nonidle conversation records are retained;
overflow remains in root counts. Completion is unseen only if it happens during
this UI session and has not been selected after completion. Old completions do
not become new unread work on startup. Seen identities are session-local, never
written into your DB. Idle/deleted/evicted records clear; normal exit clears all.
No percentage or heartbeat is fabricated. `kind=auth` uses typed saved auth/config
facts, not error-message guesses. Free text is cleaned and base64-encoded with
strict size caps; it is not terminal markup.

See [TUI terminal status](tui.md#terminal-program-status) for receiver limits and
verified scope. This feature does not change the conversation display or lifecycle.

## HTTP timeouts

Per-connection `response_header_timeout_ms` defaults to 120,000ms (two minutes) and must be positive. It bounds the interval from request start, including upload, until response headers arrive. It does not establish whether the peer received the complete request.

`stream_idle_timeout_ms` defaults to 600,000ms (10 minutes). After response headers arrive, conversation and summary streams are interrupted if this time passes between the last received chunk and the next. Conversations follow retry settings below; summaries wait for a recovery decision. Heartbeats count as received data. A response that keeps receiving data is not stopped simply because total elapsed time exceeds ten minutes. Increase the value for models with long silent periods. `0` disables only this body-idle deadline; the positive header deadline, cancellation and disconnect detection remain active.

`request_timeout_ms` is the total deadline for nonstreaming requests such as model lists and embeddings: default 60,000ms (one minute), minimum 1,000ms. Model lists use the shorter of this setting and 20,000ms, so the default is twenty seconds. A shared connection cooldown is waited before this deadline starts. Set either in the same `[connections.proxy]`, independently per connection. Restart to apply changes to the owner; standalone RAG uses settings read at invocation. Conversation idle timeouts follow the retry settings below. Summary timeouts do not automatically resend.

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
| `connections.<name>` | `api="responses"` | `"responses"`, `"chat_completions"` or `"anthropic_messages"`; catalog/embedding endpoints stay unchanged |
| `connections.<name>` | `session_header=true`, `prompt_cache_key=false` | Thread identity header and optional cache identity body field for conversations/summaries |
| `connections.<name>` | `prompt_caching=true` | Shellboy-placed cache markers (`anthropic_messages` only; nothing new for other dialects); `false` sends none |
| `connections.<name>` | `response_header_timeout_ms=120000`, `stream_idle_timeout_ms=600000`, `request_timeout_ms=60000` | Positive Responses header deadline; post-header idle (0 disables); nonstream total deadline (minimum 1000ms) |
| `connections.<name>` | `auto_at_tokens` = unset (uses `[compaction]`) | Per-connection proactive compaction threshold; 0 disables |
| `connections.<name>` | `segment_chars`, `reduce_rounds` = unset (uses `[compaction]`) | Per-connection summary part size (2000..1000000 characters) and maximum reduce rounds (1..16) |
| `compaction` | `segment_chars=64000`, `reduce_rounds=4` | Original characters per summary part (one map request each; also the reduce packing size) and maximum reduce rounds before a still-too-long summary is rejected |
| `compaction` | `auto_at_tokens=200000` | Proactive compaction when the last reported input reaches this; 0 disables |
| `connections.<name>` | `auto_trim_on_disconnect=true` | API-free maximum reduction from the second eligible consecutive failure; no preflight |
| `connections.<name>` | `response_max_retries=8`, `response_retry_max_delay_ms=60000`, `response_retry_partial=true` | Consecutive temporary conversation failure retries (0–20, 0 disables), backoff cap (1–300000ms), and retry after partial reception |
| `embedding` | `connection` / `model` = unset; `batch_max_inputs=1`, `batch_max_chars=64000` | Indexing and search API; bounded synchronous history batches |
| `shell` | `kind` = Unix `bash` / Windows `powershell`, `executable` = automatic, `env` = empty dictionary | Shell kind, executable, additional environment; not direct WSL startup |
| `retention` | `tool_raw_days=7`, `tool_raw_bytes=1073741824`, `clipboard_days=7`, `clipboard_bytes=268435456` | Raw output seven days/1GiB; managed images seven days/256MiB. Active pins/selections/drafts protected |
| `limits` | `attachment_file_bytes=20971520`, `attachment_request_bytes=41943040` | 20MiB per attachment, 40MiB per request |
| `limits` | `shell_raw_bytes=67108864`, `tool_excerpt_bytes=16384` | Shell raw 64MiB, result excerpt 16KiB; history body pages have a fixed 64KiB maximum, not a configuration key |
| `limits` | `shell_stdin_bytes=1048576`, `draft_bytes=1048576` | Stdin and draft, 1MiB each |
| `limits` | `data_warning_bytes=8589934592`, `data_max_bytes=10737418240` | Logical usage warning at 8GiB, stop new work at 10GiB |
| `cache` | `body_bytes=33554432`, `layout_bytes=16777216`, `history_bytes=33554432`, `attachment_base64_bytes=67108864` | Body 32MiB, layout 16MiB, send history 32MiB, attachment conversion 64MiB |
| `io` | `file_prepare_timeout_ms=30000`, `shell_write_timeout_ms=10000` | File preparation and stdin-write deadlines |
| `io` | `term_grace_ms=2000`, `drain_timeout_ms=1000`, `self_reply_grace_ms=250`, `shutdown_grace_ms=5000` | Termination grace, pipe drain, self-stop reply, shutdown UI grace |
| `io` | `output_buffer_bytes=8388608`, `output_batch_bytes=65536`, `output_batch_ms=25` | Raw collection buffer 8MiB, DB batch 64KiB/25ms |
| `workers` | `attachment=2`, `control_file=1` | Bounded file-worker counts |
| `indexing` | `auto_history=true`, `auto_kinds=["user","summary","assistant_response"]`, `concurrency=1`, `split_chars=2000`, `retries=2`, `common_failure_pause_count=3`, `retry_delays_ms=[1000,4000]` | Embedding concurrency, chunk characters, retries, common-failure pause |
| `ui` | `language="en"`, `max_fps=30`, `live_work_rows=3`, `held_input_rows=3`, `paste_fold_chars=1000`, `paste_fold_lines=3`, `send_key="Enter"`, `newline_keys=["Ctrl+J","Shift+Enter"]`, `copy_backend="auto"`, `palette="dark"`, `bell=true`, `program_status="on"`, `motion="on"` | UI language (`en` or `ja`), rendering, motion, independent owned-work and held-input rows, paste folding, send/newline, copy method, header/user surfaces, terminal bell on recovery wait. Keyboard enhancement is negotiated at runtime, not configured here; Ctrl+J is the portable fallback and Shift+Enter requires terminal support. |
| `logging` | `file_bytes=5242880`, `files=3` | Diagnostic logs, 5MiB ×3 |

`cache.history_bytes` exclusively budgets the cross-Thread send-history conversion LRU. `body_bytes` retains UI bodies and node metadata; `layout_bytes` budgets render layouts including widths/folds; `attachment_base64_bytes` budgets converted attachments. These are not a whole-process RSS cap including active viewport/HTTP pins, SQLite/OS caches, runtime resources, or saved history. Cache eviction does not delete history; data is fetched again as needed. A zero cache budget disables retained caching, not all active working memory.

Other numeric settings generally require positive integers. Exceptions include all four `cache.*` byte budgets (`body_bytes`, `layout_bytes`, `history_bytes`, `attachment_base64_bytes`), `connections.<name>.stream_idle_timeout_ms=0`, `connections.<name>.response_max_retries=0`, `indexing.retries=0`, and matching `retry_delays_ms=[]`. A zero cache budget disables retained caching; it does not select the default budget. These exceptions do not permit zero for `io.*` or the indexing chunk size `indexing.split_chars`. Retry delays must contain as many positive integers as retries. `wait --timeout-ms 0` is immediate CLI observation, not a configuration setting. Worker counts also have runtime-supported upper bounds.

Additional constraints: attachment_file ≤ attachment_request, data_warning < data_max, output_batch ≤ output_buffer. `shell.env` maps valid environment names to values without NUL. `copy_backend` is `auto|os|osc52`. Send/newline keys combine supported names such as Enter with Ctrl/Alt/Shift; the newline list cannot be empty or overlap the send key. `shell.kind` accepts only `bash|powershell`.

`ui.language` accepts `en` (default) or `ja` for the staged UI translation. It
is independent of the OS locale; restart the TUI after changing it. See the
[user guide](user-guide.md) for the current migration scope and terminal-width
diagnostic.

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
activation or token estimate; the proactive threshold below acts only after a
response has reported its input tokens. See [failure-driven reduction](recovery-and-storage.md#failure-driven-reduction-50).


## Stop only automatic history indexing

Set `[indexing] auto_history = false` (a TOML boolean; default `true`) and restart
the owner. Keep `[embedding]` to preserve standalone RAG semantic/hybrid search.
New conversation nodes and finalized shell raw do not authorize automatic
embeddings; automatic pending work is held, not deleted. Existing vectors stay
queryable. Already in-flight requests are not undone. Explicit backfill/rebuild,
retry of prepared failures, and probe can still incur costs. Re-enabling does not
backfill sources skipped while off: preview and explicitly select them if needed.
`index status` and `config check` expose `auto_history`. History semantic results
flag disabled automatic coverage via `degraded` / `scope_status`; keyword reads
still cover retained original text, including newer unindexed nodes.

## Choose which kinds automatic indexing takes in

`[indexing] auto_kinds` lists the kinds of conversation content that are indexed
automatically (a TOML array of words; restart the owner to change it). The default
is `["user", "summary", "assistant_response"]`: what was written to the model, the
summaries Shellboy makes and the model's replies. Tool output is by far most of
the volume, so it is not indexed unless you ask.

| Word | Content |
| --- | --- |
| `user` | Messages sent to a Thread, including messages from other Threads, and the attachment paths they carry |
| `summary` | Summaries made by `compact --summary` and automatic compaction |
| `assistant_response` | The model's replies: the answer text and the displayable reasoning text it returns (encrypted or opaque reasoning is never embedded) |
| `tool_result` | Shell command results, including the retained full output |
| `spill` | Detail moved out of the context by spill or omit |
| `event` | Developer-role notices Shellboy saves in a Thread (a finished background shell, a context change, an added attachment, and similar); most are bookkeeping, so this kind is rarely worth indexing |

Only the listed kinds are registered when they are finalized. Existing vectors are
never removed by changing the list, and work that was already queued continues.
Other kinds stay readable by `shellboy rag search --mode keyword`, and a Thread can
add them explicitly: `shellboy index preview --scope threads --thread ID --kind tool_result`,
then `index backfill` with the same arguments (see
[History and search](history-and-search.md#choosing-which-kinds-are-indexed)).
`index status` and `config check` report `auto_kinds`. An empty list indexes
nothing automatically but, unlike `auto_history=false`, is not reported as a
disabled switch. Unknown words are a configuration error.

`storage collect --category index` collects discarded partial candidates and inactive spaces with no vectors only (`index remove` deletes a retained index); turning automatic indexing off does not free active-index bytes.
There is no CLI to erase all active vectors while retaining history; do not delete SQLite tables or the database manually.

## Explicit summary size limits

`compact --summary` maps chronological parts and consolidates them into one
summary. It removes repeated goals/constraints, requests the latest confirmed
state, and retains decisions/reasons, uncertainty, unresolved work and references.
These prompts are not a guarantee of factual correctness; inspect sources when needed.

```toml
[compaction]
summary_max_chars = 20000
summary_max_percent = 5
summary_map_max_requests = 128
summary_reduce_max_requests = 16
```

Without this section, both limits above apply. In a present section, specify
one or both keys: omitted keys impose no additional limit. With both, use the
smaller of the absolute limit and percentage of the original range's projected
characters, floored at 2,000 Unicode characters. Character limits must be positive;
percentages must be finite and in (0, 100], including fractional values. An empty
section is invalid. This floor does not permit a summary to enlarge its original
source projection: such a result is rejected too.

`compact --summary --from FIRST --to LAST --max-chars 6000 --max-percent 2.5`
replaces the corresponding configured limits for that invocation; unspecified
limits keep their configured values. Run `config check` to validate the section
and inspect its resolved values in `result.compaction`, then restart the owner
to apply changed settings (the proactive threshold can also be changed per Thread
without a restart; see below). Checking still uses only the model catalog, not a chat.

Map and reduce have independent request budgets: `summary_map_max_requests`
(default 128) and `summary_reduce_max_requests` (default 16), each in 1..4096.
Before sending any model request, count the known uncached map segments. If they
exceed the map budget, refuse with total/uncached/cached segment counts and a hint
to narrow the range or raise the map budget. Valid cached segments consume no
map requests, and changing an output-size limit does not change map cache keys.
Unexpected provider context-limit splits consume the remaining map attempt budget.
Reduce still has at most four rounds and its own request budget. A section with
only request-budget settings uses the default 20,000-character/5% size limits. Over-limit summaries are reduced more
strongly, never truncated to fit. On failure, no replacement is adopted; inspect
`source_characters`, `summary_characters`, `summary_limit` and `model_requests`
in the error details. Successful downstream results expose these under `compact`
in addition to before/after context sizes. Cached successful parts can be reused
on explicit retry, so model_requests counts requests actually made this time.
Automatic failure stage 2 and argument-free `compact` use the same bounded
summarizer. An over-limit automatic summary is not adopted; existing #50 recovery
handling enters recovery wait. The prior spill stage may already be saved.

Omitted history can stay omitted. Use bounded keyword RAG/`history get` first;
when embeddings are configured, `history search` can help with differently worded
facts but sends a potentially paid query embedding. Only summarize omitted
history to reintroduce key facts. Summarizing an omitted range can
increase the selected context even when it is much shorter than its originals.

## Proactive compaction threshold

```toml
[compaction]
auto_at_tokens = 200000   # default; 0 disables

[connections.proxy]
auto_at_tokens = 120000   # optional per-connection override; 0 disables
```

Shellboy keeps conversations small during normal operation. It never blocks or
pre-checks a large input: the request is sent. When the provider reports in that
response's usage that the input was at least `auto_at_tokens` tokens, Shellboy
runs the argument-free `compact` (spill, then bounded summary; never maximal
omission) at the next safe boundary, before the following request. Reaction is
therefore one request late by design. Only the provider-reported count is used;
there is no estimate, and providers or gateways that report no input tokens never
trigger it. Context-overflow reduction remains the last-resort fallback.

If the compaction is not adopted (summary failure, nothing safely compactable),
the request continues. Another attempt is made only when the saved history changes so that the bounded
candidate forecast shows new or grown eligible content (a new spill group or
summary source) compared with the saved evidence; a later Turn alone, or more
input tokens alone, does not retry. If a spill succeeds but
the summary is rejected, `compact list` records an adopted spill with
`metrics.summary_rejected: true` and the failed summary's reported usage; the
same eligible-content gate applies before another attempt. Proactive spill selects only
safe groups whose replayed JSON becomes shorter; this is not a token estimate.
Ordinary summary protects
the current and latest completed Turns; a long current Turn's old tool outputs
can be spilled, but its text is not included in that summary.
`compact list` shows each attempt as `automatic_reduction` with
`reason: "proactive_threshold"`, its `state`, and `trigger.auto_at_tokens` /
`trigger.input_tokens`. Summaries call the model and may cost tokens; compaction
can also reduce prompt-cache hits. Each event's `metrics.summary_usage` and the
`compaction` section of `shellboy usage get` show the provider-reported tokens of
those summary requests (input, cached, cache-creation, output), separate from
conversation requests, so you can judge whether the threshold pays off. Values a
provider does not report stay unknown; reused cached summary parts send nothing. To keep a persistently large context, set
`auto_at_tokens = 0` globally or for one connection, then restart the owner.
To change the threshold for one Thread without a restart, use
`shellboy thread set --auto-compact-at N` (`0` turns proactive compaction off for
that Thread, `''` returns it to the connection / `[compaction]` value). The Thread
value wins over the configured ones, applies from the next request-boundary check
(never to a request already sent), stays when the Thread's connection changes and
is inherited by child Threads; `thread get` shows it as `auto_compact_at` beside
the value in use, `effective_auto_compact_at`.

### Summary part size and reduce rounds

```toml
[compaction]
segment_chars = 64000   # default; 2000..=1000000
reduce_rounds = 4       # default; 1..=16

[connections.proxy]
segment_chars = 32000   # optional per-connection override
reduce_rounds = 6       # optional per-connection override
```

`segment_chars` is how many characters of the original conversation go into one
summary part (one model request) and the size reduce requests are packed to;
`reduce_rounds` is the most reduce rounds before a summary that is still over its
limit is rejected. The value in effect is the first one set of: the single summary's
`compact --summary --segment-chars N --reduce-rounds N`, the Thread's
`thread set --segment-chars N --reduce-rounds N` (`''` clears), the connection's,
then `[compaction]`. Out-of-range values are rejected at every level. A conversation group larger than `segment_chars` cannot be split: the summary stops before any request, so with a small value one long message or unspilled tool result makes every automatic summary for that Thread fail until the value is raised (`compact --dry-run` shows the refusal before any cost). A smaller value also means more reduce requests and rounds, which can reach `summary_reduce_max_requests` or `reduce_rounds` before the map budget. Parts made at
one size are not reused at another size except where the covered nodes coincide. Each
`compact list` item records the values it used (`metrics.segment_chars`,
`metrics.reduce_rounds`, with `metrics.summary_limit`), so a change can be compared
with the earlier results; see the Agent Guide for the method. Thread values are
copied by child Threads and forks and survive connection changes; `thread get`
shows `segment_chars` / `reduce_rounds` beside `effective_segment_chars` /
`effective_reduce_rounds`.

### Unknown shell arguments

`[shell] unknown_arguments = "warn"` is the default: unknown model-supplied
shell parameters are ignored and named in the call result. Set it to `"strict"`
to reject calls containing unknown parameters. Invalid known parameters are
errors in either mode.

### Shell call descriptions

`[shell] describe_calls = true` is the default and requires `description` in
generated shell tool definitions. Received missing/blank descriptions still use
command fallback. Execution/display descriptions are limited to 80 Unicode
characters; saved/replayed model arguments remain verbatim.
Descriptions are model-authored, not command verification. False
omits the argument from generation tool definitions and displays commands instead.
Saved history and command provenance are not deleted by opting out.

### Optional Responses Cache Diagnostics

Set `connections.<name>.prompt_cache_diagnostics = true` only on a Responses
connection with `prompt_caching = true`. The default is false. When enabled and
a usable previous adopted response ID exists in the context display's current
connection/model scope within the same Thread's 33 most recent Turns, Shellboy
sends it as `prompt_cache_options.comparison_response_id`. No matching attempt
in that finite window, or a missing ID on the selected attempt, omits the entire
options object; there is no older-window or older-ID fallback. Selection uses
saved rows across Turns/restarts; failures and cancelled/stale results are not
adoptions. Forks have no own adopted attempt. No new schema or sticky state is
created and no provider-specific compatibility rule is guessed.

`prompt_cache_options` is now a Shellboy-owned dynamic Responses body field;
it cannot be set in `extra_body` (including for a static TTL). No sticky headers
are captured or forwarded. This feature does not guarantee that a provider or
proxy accepts the options, preserves IDs, or returns diagnostics.

Inspect a saved attempt with `shellboy usage get --attempt ATTEMPT_ID`.
`prompt_cache_diagnostics` contains only reported `type`, `reason`, and
`cache_missed_tokens`, or JSON null when unavailable. Strings/numbers are
whitelisted and bounded; invalid/oversized members are discarded, not guessed
or truncated into new provider facts. This does not calculate cache misses from
usage counters. `shellboy thread request --thread THREAD_ID --attempt ATTEMPT_ID`
also shows the observation under `saved.prompt_cache_diagnostics`, and rebuilds
the saved comparison ID instead of borrowing the Thread's current last ID.

## Terminal Ambiguous Width

Shellboy automatically measures the terminal's ambiguous-character width.
It remembers successful observations per terminal identity and never saves a
guessed result. When no observation is available, it uses conservative
fallback glyphs and reserves wider character widths.
