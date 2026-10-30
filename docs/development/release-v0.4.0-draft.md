# Shellboy v0.4.0

Noninteractive JSON answers, easier setup and installation, more connection
choices, and clearer conversation and terminal status.

## Before upgrading

- **Stop the owner and back up the complete data directory first.** Conversation
  DB schema stays14 and standalone RAG stays3; this release adds no migration.
  Keep the backup even when the schema number does not change. Restore a backup
  into a separate data directory if you need to roll back.
- **Use the same build for owner and CLI, then restart the owner.** The local
  protocol remains2, but `exec` needs the new owner's final-answer read support.
  It checks before submitting work and exits6 on an incompatible owner; if
  support disappears later, inspect the reported IDs instead of sending again.
  Replacing an executable does not update an already-running owner.

## Install and start configuring

- **One-line installation is available from this release.** Release-bound
  `install.sh` and `install.ps1` verify archive SHA-256, install without sudo and
  keep a previous binary as a backup. Use `SHELLBOY_INSTALL_DIR` to choose the
  destination or `SHELLBOY_VERSION` to select a version. The scripts do not change
  PATH or shell profiles; they print any required next steps. Windows replacement
  also preserves a running executable and supports rollback on rename failure.
- `config init` writes a new commented template without overwriting an existing
  file. Edit its reported path, set credentials in the environment, then use
  `config check` and start the owner with the same `--config` path. Missing or
  placeholder settings stay visibly unfinished. Catalog checking is not a paid
  chat probe; an unadvertised model can be an alias and is a warning, not proof
  that the model is unavailable. No OAuth, keychain or setup wizard is added.

## Ask for work and get JSON

- `shellboy exec --schema result.schema.json "Inspect the repository"` combines
  normal saved work, waiting and final-answer retrieval. By default it creates
  a saved conversation; `--thread` selects an existing one. Normal shell/tool
  work is still permitted; this is not a tool-free transform or a sandbox.
- The **local validator** checks the final answer. stdout contains only the
  validated JSON; IDs, repair count and saved Turn usage go to stderr. A wrong
  answer can be corrected in the same conversation, with a small retry cap.
  Timeout leaves the work running and reports IDs for inspection or explicit
  stopping. The default self-asserted sender label is `shellboy exec`.
- The initial Schema subset covers types, object/array structure, required
  properties and length/count checks. `$ref`, combinators, enum/const, patterns,
  formats and numeric constraints are explicitly rejected before submission.
  This is not a promise of full JSON Schema support or factual correctness.
  Distinct exit codes and PowerShell UTF-8 examples are in the CLI reference.

## Connections and saved input

- Connections can select `api = "chat_completions"` in addition to the default
  Responses API. Standard text/tool exchanges, usage facts and normal recovery
  share the same saved history. **Real providers remain unverified.** Vendor
  reasoning/opaque tool metadata and provider-specific extra-body/cache controls
  are not supported by this initial adapter. Gemini examples are unverified,
  not compatibility or cache-effectiveness claims.
- **Anthropic's OpenAI-compatible facade is not a supported target.** Its stated
  comparison/testing purpose, lack of prompt caching there and reasoning-control
  limitations make it unsuitable to recommend for long conversations. Use a
  suitable native-API proxy instead; no native Anthropic/Gemini API is added.
- `message get`, history provenance and F6 expose credential-free sender facts.
  New external CLI input is not labelled as a verified human. Optional
  `--sender-label` is a self-asserted name, never permission to stop/retry/own work.
  Old missing evidence stays unknown in inspection; old conversation bands keep
  their established appearance.
- Finished submitted-draft copies are collected after known admission or
  cancellation. Small receipt facts preserve replay and autosave protection;
  uncertain/submitting copies and cancellation archives remain available.

## Conversation view, terminal status and guides

- Shipped conversation-view units provide clearer user bands/header and compact
  tool rows, bounded real output tails and shared details/copy navigation.
  Current F6/F7 facts can refresh while identified paid output remains a
  timestamped snapshot. The broader #41 redesign is not declared complete.
- OSC7501 program status is on by default. `ui.program_status = "off"` disables
  all reports, queries and exit clears. Root counts and bounded conversation
  records describe actual saved state, not fabricated percentages/heartbeats;
  unread completion is session-local. Receiving-terminal compatibility remains
  limited to the documented validation scope.
- Guides include a disposable, qualified macOS sandbox recipe and explicit
  limits, plus child-conversation cleanup. Other OS recipes remain deferred
  or conceptual where not tested. Models still use the owner's OS access by
  default; this release does not introduce general containment.

## Downloads and qualification

Windows x64, macOS Apple Silicon and Linux x64 archives contain the executable,
LICENSE, configuration example and offline guides. Verify the three archives and
both version-bound installers against `SHA256SUMS` before use.

Candidate SHA, exact-source CI/build/installer checks and the checksum-verified
public0.3.0 synthetic upgrade are tracked in issue70. Only loopback mocks are
used, not user databases or paid providers. Cross-OS native startup/install
checks do not establish real-provider or every-terminal qualification.
