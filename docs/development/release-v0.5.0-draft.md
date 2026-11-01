# Shellboy v0.5.0

A rebuilt conversation view, short explanations for shell work, terminal-free
owners, and a clearer breakdown of saved model usage.

## Before upgrading

- **Stop the owner and back up the complete data directory.** Conversation
  storage advances from schema 14 in v0.4.0 to schema 17. First write-capable
  startup adds derived readback, observation and parent-lookup indexes; standalone
  RAG remains schema 3. Older binaries cannot open the upgraded conversation
  database. To roll back, restore the backup into a separate data directory.
- **Update owner and CLI together, then restart the owner.** The local protocol
  remains 2, but new commands and read options require the matching build.
  Replacing an executable does not update an owner that is already running.
  Before 1.0.0, compatibility with earlier releases is not guaranteed.

## Conversation view

- The E/F/G conversation-view work separates saved facts from rendering, pages
  long histories and reuses bounded display caches. Scrolling, current work,
  details and browsing share more of the same saved observations instead of
  repeatedly rebuilding the conversation during a draw. This is not a claim
  that all database reads are now off the UI thread or constant-time.
- Input bands, headers and external-sender marks are clearer, including wide
  characters and terminal-edge backgrounds. The footer distinguishes request
  phases, context-edit waits, elapsed time and saved completion times. Accepted
  model attempts become visible before transport headers arrive.
- Owned work, saved notices and original-source links make it easier to inspect
  the exact shell, response or event. Contextual shortcuts and code-block choices
  refer to canonical saved source; display captions are not copied as code.
- The broader #41 redesign is **not declared complete**. Follow-up list/status,
  retained-source and confirmation/external-observation work (#107-#109), plus
  bounded-read work (#115), remain separate. Unmerged work is not included here.

## Shell descriptions

- Tool rows can say what a shell command does rather than show only its command
  text. Models supply a brief `description` in the conversation's language,
  limited to 80 characters for execution/display. Details retain access to the
  actual command and its exact call identity.
- `[shell] describe_calls = true` is the default and requires `description` in
  generated tool definitions. Set it to `false` for command-based rows and a tool
  definition without that parameter. Received calls with missing or blank labels
  still execute with command fallback; saved/replayed arguments stay verbatim.
  Descriptions are model-authored labels, **not command verification or consent**.

## Terminal-free owners

- `shellboy serve --data-dir DIR [--config FILE]` runs an owner without an
  interactive terminal. The explicit directory uses the normal exclusive owner
  lock. Wait for stdout JSON `event: "ready"` before sending CLI requests.
- `shellboy shutdown --data-dir DIR` requests cleanup of that serve instance.
  Acceptance is not termination: observe process exit and the final stdout
  `event: "stopped"` row. Unix INT/TERM/HUP use graceful cleanup; Windows uses
  the shutdown command, not CTRL_BREAK. A stale request cannot stop a replacement
  owner, and TUI owners reject this command.
- This is **not a daemon or a later-attach UI**. The process stays owned by its
  caller, including a model's background shell/Run. Stop nested owners when done.
  Restart does not automatically resume interrupted work or transfer live tools.

## Saved usage and inspection

- `usage get --group-by hour|day|thread` shows saved request-size mean, maximum,
  p50/p95, known/unknown counts and paired cache ratios. Maximum-request references
  lead back to its Turn/Run/Thread. Use `--since`, `--until` and `--timezone` to
  select a period; grouped reads default to the last 24 hours across non-deleted
  conversations, whereas ungrouped usage keeps its existing scope behavior.
- Reads are bounded and paginated. Follow cursors even after an empty filtered
  page. `scan_truncated` means a partial candidate window: groups can recur, and
  page percentiles/ratios must not be averaged into whole-period estimates.
  Missing usage is not zero; these are saved facts, **not invoices or prices**.
- `shell list` finds saved executions by command, scope, time, state or exit code;
  use returned execution IDs with `shell get`/`shell read` for actual output.
  `compact list` exposes saved compaction facts, and usage can be filtered by
  saved-start periods. These inspection reads do not start work or call providers.
- Explicit `compact --summary` accepts `--max-chars` and `--max-percent` bounds.
  Automatic history embedding can be disabled with `indexing.auto_history=false`;
  existing vectors and standalone RAG are not removed or disabled.

## Connection-specific request settings

- Connections can set `extra_body` and `extra_headers` for provider-specific
  options. Shellboy forwards them without interpreting their meaning. Extra
  body fields never overwrite Shellboy-owned fields; reserved-key collisions,
  invalid values and size limits fail configuration checks. Extra headers cannot
  carry credential-like or transport-owned names; keep keys in `api_key_env` or
  the existing credential settings. Extras apply to conversation/summary requests,
  not embedding or model-list requests. Attempts retain an extras hash, not a
  copy of their contents (#119).
- Set `context_role = "user"` on a connection when using **Claude through
  LiteLLM**. This is recommended for gateways that fold developer messages into
  the system prompt and then reject an assistant-ending conversation. The default
  remains `"developer"`. The setting maps post-system developer context to user
  role at send time, without changing its text or saved history. Restart the
  owner after changing connection settings (#121).
- The dialect-module reorganization is internal and keeps existing Responses and
  Chat Completions behavior. Only #120 stage 1 is included: native Anthropic
  Messages and the later opaque provider-block work are **not added** by this
  release. Connection settings are not a real-provider/cache-effectiveness
  guarantee; provider-specific options remain the user's responsibility.

## Other improvements

- `wait` can watch several Threads settling and resume from a saved cursor, so
  completion/recovery transitions between calls are not silently missed. A
  satisfied condition still requires inspecting the saved result; it does not
  prove success or authorize retrying work.
- Attachment observations expose managed local paths with availability facts,
  and `status --memory` reports owner cache accounting and native process memory
  on demand. These observations neither grant additional OS access nor promise
  that cache bytes account for all process memory.
- Connection-wide quota cooldowns use explicit usage-reset evidence and prevent
  other conversations from immediately repeating a known-blocked request. This
  does not infer billing, repair provider usage or guarantee a real endpoint's
  behavior. Typed slash commands also retain pasted arguments correctly.

## Fixes and development

- List/Watch viewports no longer hide promoted running rows until reopening
  (#100). Committed stop requests settle after driver ownership retires (#106).
- Scrolling back up near the live tail no longer gets stuck after reversing
  direction. Pending history loads preserve navigation anchors and show loading
  feedback instead of losing the requested position (#117).
- Context-trim markers use the outbound notice's actual size and summary (#112).
  Shell previews follow visible disclosure and output revisions, avoiding stale
  preview state after equivalent metadata changes (#99).
- Header/input band extents and wide-cell rendering are corrected (#91), while
  footer phase/elapsed labels and attempt-admission notifications agree with saved
  state (#85). Contextual copying and exact saved-notice links are improved.
- Development changes add disposable terminal fixtures, read/query-count gates,
  reusable repository helpers and an optional read-only shell-error catalogue.
  Development builds retain line tables rather than full debug information.
  These do not add a runtime service, containment boundary or provider guarantee.

## Downloads and validation

Windows x64, macOS Apple Silicon and Linux x64 archives include the executable,
LICENSE, configuration example and offline documentation. Verify all three
archives and both installer scripts against `SHA256SUMS`.

These notes include #101, #117, #120 stage 1, #119 and #121 from main `a3e00bed`.
Release-preparation CI, candidate archives/installers/checksums and final-candidate
upgrade checks are
separate gates; see the v0.5.0 section in `publication.md` for their status.
No tag, GitHub Release or installed-binary replacement is performed by this
preparation. Candidate builds and actual publication remain reviewer-owned.
