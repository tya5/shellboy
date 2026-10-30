# Shellboy v0.4.0

Run saved work from scripts, get a checked JSON answer, and start configuring and
installing Shellboy more easily.

## Before upgrading

- **Stop the owner and back up the complete data directory.** The conversation
  database stays at schema 14 and RAG stays at 3; this release has no database
  migration. Keep the backup even when the schema number does not change. To
  roll back, restore your backup into a separate data directory.
- **Use the same build for owner and CLI, then restart the owner.** The local
  protocol remains 2, but the new `exec` command requires the new owner. `exec`
  exits with code 6 before sending work to an old owner. If support disappears after work
  has run, use the reported IDs to read its answer instead of sending it again.
  Replacing a file does not update an owner that is already running.

## Install and configure

- **One-line installation starts with this release.** `install.sh` and
  `install.ps1` download the matching archive, verify SHA-256, install without
  sudo and keep an existing binary as a backup. Choose a temporary or custom
  destination with `SHELLBOY_INSTALL_DIR`, or a version with `SHELLBOY_VERSION`.
  The scripts do not edit PATH or profiles; they print the next steps.
- `config init` creates a commented template without overwriting a file. Edit
  the reported path, set credentials in the environment, use `config check`,
  then start the owner with the same `--config`. Missing/template settings stay
  visibly unfinished. Checking the model list is not a paid chat probe, and an
  unlisted model name can be a proxy alias rather than an error.

## Saved work with a JSON answer

- `shellboy exec --schema result.schema.json "Inspect the repository"` sends
  ordinary saved work, waits and checks its final JSON answer locally. It makes
  a new saved conversation by default; `--thread` uses an existing one. Normal
  shell/tool work is allowed: this is not a tool-free transform or a sandbox.
- stdout contains only the checked JSON. IDs, repair count and saved Turn usage
  go to stderr. A wrong answer can be corrected in the same conversation, with
  a small limit. Timeout does not stop work; inspect the IDs or explicitly stop
  the Run. **For scripts and CI, run `config check` first to check the connection.**
- The first Schema subset supports types, object/array structure, required
  fields and length/count checks. Unsupported features, including `$ref`,
  combinators, enum/const, patterns, formats and numeric constraints, are rejected
  before submission. This is not full JSON Schema support or a guarantee that
  an answer is factually correct. Exit codes and PowerShell UTF-8 examples are
  in the CLI reference.

## Connections, senders and drafts

- Select `api = "chat_completions"` as an alternative to the default Responses
  API. Standard text/tool exchanges, saved usage and recovery use the same
  conversation history. **Real providers remain unverified.** Some providers'
  reasoning/tool extensions and extra cache controls are not supported. Gemini
  examples are not compatibility or cache-effectiveness claims.
- **Anthropic's OpenAI-compatible facade is not a supported target.** Its stated
  testing purpose, lack of prompt caching there and reasoning-control limits
  make it unsuitable to recommend for long conversations. Use a suitable native-
  API proxy instead; native Anthropic/Gemini APIs are not added here.
- `message get`, history and F6 show saved sender IDs without credentials.
  External CLI input is not treated as a verified human. `--sender-label` is an
  optional self-asserted name, never permission to own, retry or stop work. Old
  missing evidence remains unknown. `exec` names itself `shellboy exec` by default.
- Redundant draft copies are removed after confirmed delivery or cancellation.
  Enough information remains to prevent duplicate sending or old autosaves
  returning. Uncertain/unfinished submissions remain recoverable, and cancelled
  text and attachments can still be restored without sending.

## Conversation view, terminal status and guides

- Shipped conversation-view changes include clearer bands/header, compact tool
  rows, bounded real output tails, and shared details/copy navigation. F6/F7 can
  refresh current facts; saved output remains a dated snapshot. The
  broader #41 redesign is not declared complete.
- OSC 7501 terminal status is on by default. Set `ui.program_status = "off"` to
  disable all its output. Counts describe saved work, not guessed percentages;
  unseen completions belong to the current UI session only. Receiving-terminal
  compatibility remains limited to the documented checks.
- Guides include a tested disposable macOS sandbox example and its limits, plus
  cleanup of delegated conversations. Other OS recipes remain deferred or
  conceptual where untested. Models still use the owner's OS access by default;
  this release does not introduce general containment.

## Downloads and verification

Windows x64, macOS Apple Silicon and Linux x64 archives include the executable,
LICENSE, configuration example and offline guides. Check the three archives and
both install scripts against `SHA256SUMS` before use.

Candidate SHA, CI/build/install checks and the public 0.3.0 synthetic upgrade are
recorded in issue #70. Tests use loopback mocks, not user databases or paid
providers. Native startup/install checks do not qualify every terminal or real
provider. Tagging and publication remain separate approval steps.