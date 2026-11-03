# UI language foundation (#139 / #35 stage A)

Status: implementation proposal, subject to review and the validation recorded
in [validation](../validation/acceptance.md#ui-language-and-terminal-width-foundation-139-2026-11-01). The effective contract is in
[effective spec](effective-spec.md#display-language-and-terminal-width-foundation-139--35-stage-a).

## Language boundary

`[ui] language` is `en` or `ja`; `en` is the default. It does not consult
`LANG` or `LC_*`, and unsupported values fail configuration validation. Resolve
one complete `Message` per UI sentence with typed parameters: English zero,
one and many can have different forms, and Japanese can use a different word
order. Stage A includes the force-quit confirmation and width diagnostic; the
other screens remain stage B. User/model text, provider errors, CLI and protocol
values, content submitted to models, and persisted content are not translated.

A small exhaustive Rust enum is preferable here to a new catalogue dependency:
two locales and a small initial surface fit compile-time coverage and typed
parameters. It makes translators touch code and requires explicit plural rules;
re-evaluate a structured catalogue such as Fluent before adding many locales or
complex plural/gender rules. Do not add an ambient-locale fallback.

## Copy worker UI boundary (#152 review correction)

The bounded copy worker's `CopyError` messages include TUI actions (`o`, `n`,
`b`); they are complete UI messages, not locale-independent Core diagnostics.
Resolve them using the immutable startup `control.store.config.ui.language`,
the same loaded `[ui] language` used to create `UiState`, and carry that value
through source pagination and presentation work. Default En uses English with
ASCII parentheses; explicit Ja retains the pre-#152 Japanese sentences exactly,
including their full-width parentheses. Source reasons remain verbatim typed
arguments. Do not localize error codes, hints/details from another subsystem,
source hashes, copied bytes, code indices, pagination or limits.

Current production entry points are TUI `LoadCopyChoices` and `CopyOriginal`;
they deliver errors through `UiUpdate` / `BackgroundResult` and the session-only
command outcome. The public Rust `frontend::copy_source` helper shares the TUI
worker and is currently used by injected frontend tests, not by CLI dispatch or
IPC/wire routes. Repository call-site inspection found no CLI/wire exposure of
these `CopyError` values. No CLI locale behavior is changed by this correction.
If a CLI/wire copy route is added, give it an explicit English non-TUI diagnostic
boundary instead of reusing localized key guidance. Underlying History/API
errors continue to propagate unchanged; do not translate their source text.

## Terminal width

Implementation pending; this paragraph records the adopted target, not current behavior.

On every startup, send width, device attributes (DA), and keyboard enhancement
queries before the first frame without waiting. One capability-probe mechanism
tracks each as `Unmeasured` or `Measured(value)`. A single input reader removes
DSR (`ESC [ row ; column R`), DA (`ESC [ ? ... c`), and keyboard flags
(`ESC [ ? ... u`) replies at any time, including while the user is typing.
Replies never become input. No response leaves the capability unmeasured
indefinitely. A successful width measurement becomes known and causes one
redraw when layout changes. The last successful width per terminal program and
version (or TERM) is only an initial baseline; a new successful measurement
refreshes it. Unknown identity and guesses are never cached. If no cache exists,
use conservative fallback glyphs and widths. No user width setting exists.
Neither locale nor terminal name infers width. F1 help reports the result;
diagnostics record measured mismatch with upstream Ratatui.

Shellboy layout uses the observation, with unknown conservatively reserving two
cells for ambiguous characters and component widths for emoji sequences.
Ratatui and upstream renderers retain their own default width behavior, so on a
mismatching terminal their internal layout can differ from Shellboy's. Shellboy
must use width-invariant ASCII decorations for a two-column or unknown observation; existing direct `SELECTED` and `CHILD` glyph
uses do not yet meet this requirement and must be corrected. This does not
guarantee alignment of preserved user/model text on every terminal;
when one column is measured it restores the original application-owned glyphs.
Raw content and copy/save paths do not change. A pluggable Ratatui width policy
is separate follow-up work.

Application-owned ambiguous symbols and renderer-generated Markdown decorations
use their original glyphs only after a one-column measurement; otherwise they
use ASCII alternatives. Generated emoji alert icons use conservatively computed
Unicode widths rather than being assumed to occupy one cell. Markdown table
borders carry a private style marker, so fallback conversion affects only the
renderer-generated border; a user-supplied identical character is not converted.
Source anchor mapping removes the tagged output guide, rather than stripping
ASCII pipes from user text.

## Stage B partition proposal

Each slice owns its message submodule and 80/120-column bilingual fixtures;
dynamic values are typed arguments of complete messages. The shared `src/ui.rs`
integration points are owned by the integrator after each slice is reviewed,
rather than edited concurrently by every slice.

1. Help, command reference, shortcuts and key hints: `src/ui/help.rs` and
   `src/ui/key_hints.rs`.
2. Conversation, run, tool and recovery presentation: `src/ui/presentation.rs`,
   `src/presentation/` and `src/ui/history_interaction.rs`.
3. Thread/browser, watcher and live-work views: `src/ui/threads.rs`,
   `src/ui/watch.rs` and `src/ui/live_work.rs`.
4. Composer, draft, queue and copy interactions: `src/ui/input.rs`,
   `src/ui/draft.rs`, `src/ui/message_queue.rs` and `src/ui/code_copy.rs`.
5. Confirmation, deletion, model, settings and management overlays: remaining
   `src/ui/` overlay modules. Force quit is the stage A example and must not be
   remigrated by this slice.
6. Startup, frontend and command diagnostics that are actually application UI,
   followed by bilingual guide and acceptance updates. Protocol, persisted
   content, program output and provider/model text remain unchanged.

Check each split against current main before assigning parallel work; shared
registries, `src/ui.rs`, broad snapshots and guide updates are integration-owned.

## Common layout and test rules

Measure translated complete strings in terminal cells, not bytes or UTF-8 code
points. Derive a screen's label/content width from its actual localized lines;
clamp it to the available viewport and reserve borders, padding and scrollbar
cells separately. `popup::text_viewport` implements this for the stage A sample.
Do not use a fixed English/Japanese label column. Recalculate after resize.

When space is constrained, preserve in order: the consequence/safety warning
and destructive scope; explicit confirm/cancel controls; the selected item or
identity; current state and failure/recovery facts; then secondary context,
counts, timestamps, hints and decoration. Wrap or scroll the body before hiding
an action. Truncation is display-only, on a grapheme boundary, with `…` after a
one-column measurement or ASCII `.` otherwise; retain the complete
source/copy/execute value. For English text, wrap
long words at grapheme boundaries if necessary; never cut a critical confirmation
word when 80/120 columns are available. Status-bar and section rows use the same
priority: keep state and action, drop decoration and optional detail first, and
keep overflow discoverable in the existing detail surface.

For stage B, register each synthetic screen with `ui::i18n::verify_screen`:
provide setup and the essential complete phrases for both languages. It renders
80/120 columns for measured width one/two and unknown, checks generated symbol
width invariance, emoji characters using the test-only Unicode emoji property dependency,
popup borders and essential phrases. Fixtures containing user
or model data need separate source/copy-preservation assertions; never sanitize
that data merely to satisfy a symbol test.

## Watch page notice boundary (#152)

The Watch discovery worker owns two complete UI notices: unavailable descendant
scope and a bounded-search continuation hint. Capture the loaded UI language in
`watch_page::read` and pass it only to notice rendering during collection.
Default En uses English; explicit Ja preserves both pre-batch sentences exactly.
The supplied notice travels through `UiUpdate::Threads` to the browser's existing
localized filter/status line. No CLI/wire error or saved value is localized here.

Locale is deliberately excluded from the request, seen hash, watermark, sort key,
cursor encoding and selection sequence. Do not translate stored titles, source
text or facts that happen to contain the same words as a notice. Bounds,
continuation guards, cancellation and read-only snapshot behavior are unchanged.

## Review Fingerprint Scope

Reviewed language-audit literals bind to their enclosing function or standalone
const/static item, including item attributes when recognized; unrecognized
contexts fall back to the whole file. The full enclosing function deliberately
includes language predicates and branches. This reduces invalidation by sibling
items but does not track callers, helper/macro definitions, enclosing module or
impl attributes, or behavior changed without local token changes. Fingerprints
are re-review prompts, not proof; cross-item dependencies and duplicate literal
ordinals require human inspection. The CI consistency check rejects stale or
invalid reviewed decisions, not the current nonzero (c) inventory.

## Deletion cleanup storage-warning boundary (#152)

`frontend::thread_deletion::finish_cleanup` supplies a complete TUI warning after
Core deletion has already succeeded. Only its application-generated storage-
failure wrapper follows the immutable loaded UI language. Default En is English;
explicit Ja retains the original sentence exactly. The underlying `ApiError`
display is an opaque argument: preserve its message, code, hint/details and bytes.
Other error kinds pass through unchanged. This is not a Core/CLI diagnostic change.

The existing `DeletedWithWarning` UI consumer already localizes its outer status.
Do not change confirmed deletion, tombstone observation, local rollback, admission
closure/resource reclamation, ordering, limits or shutdown results. Panic/queue
messages requiring deferred root changes are not part of this one-warning batch.
Neither #150 nor #157 production ranges are edited.

## Draft Worker Stopped Notice Boundary (#152 Batch6)

After confirmed #150 landing at main89c570a1 (exact3OS CI41399417402), review the
current draft_saver leaf rather than replaying the unpublished pre150 prototype.
Only pump_delivery's complete worker-stopped notification follows the immutable
loaded startup UI language. Explicit Ja keeps its exact original sentence; En
uses complete English. Existing #150 rejected_message(language), recovery advice
and callers remain intact. No root/composer/Watch/other #157 range changes.

Capture the language in DraftSaver::new without altering pump admission, reserved
main delivery, pending-failure ownership, coalescing, once-per-Thread warnings,
raw snapshots/anchors/revisions, shutdown/error results, persistence or limits.
No source text, user reason or stored draft is translated. Remaining candidates
and the old prototype are not adopted by this one-notice boundary.

## Bounded Recovery Hint Language (#152)

For private Run/Turn inspection and management views, the synthetic hint for a
saved recovery reason exceeding the existing 64 KiB read budget follows the
loaded UI language. Japanese retains the existing sentence. Ordinary saved
reasons, source bytes, limits, context values and CLI/wire responses are not
translated or rewritten. The language is captured with the private observation;
this does not alter recovery or loading behavior.

## Index Observation Copy (#152)

Index observation prioritization follows the selected UI language for its
application-generated labels, count confidence, null fallback and coverage
navigation text. Counts retain their exact / lower-bound / unknown distinction.
Provider reasons and other source strings are not translated. Pointers, opaque
cursors, underlying values, anchor derivation and the existing bounded projection
remain unchanged. Empty-key details are retained by key, not inferred from a
translated or caller-supplied label.
