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

After raw mode and the alternate screen are active, an isolated helper process
uses the public crossterm `cursor::position()` API to measure one and two
`U+00B7` characters on row 11, before the event loop starts. The parent waits
at most 300 ms and kills and reaps the helper on timeout. Isolation prevents a
stuck cursor query from holding the UI process's event-reader lock. Crossterm
may buffer non-position events in the helper during the probe; those events are
not transferred back to the UI, including when the helper times out. Input
arriving during the bounded startup interval can therefore be lost. Two agreeing
responses identify one or two; a timeout, unsupported input path, small terminal,
error or disagreement is unknown. No terminal name, locale, override, or user's
choice of language determines width. F1 help reports the result. When measured
two differs from upstream Ratatui's default of one, TUI status and optional
diagnostics explicitly record the mismatch.

Shellboy layout uses the observation, with unknown conservatively reserving two
cells for ambiguous characters and component widths for emoji sequences.
Ratatui and upstream renderers retain their own default width behavior, so on a
mismatching terminal their internal layout can differ from Shellboy's. The
impact is limited to content rows; generated UI symbols remain width invariant.
Raw content and copy/save paths do not change. A pluggable Ratatui width policy
is separate follow-up work.

Application-owned ambiguous symbols use ASCII alternatives. Existing semantic
row marks/spinners remain only if their narrow and CJK widths agree. Markdown
borders are renderer-generated decorations: they carry a private style marker
and are converted to ASCII before drawing; a user-supplied identical character
is not converted. Source anchor mapping removes the tagged output guide, rather
than stripping ASCII pipes from user text.

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
an action. Truncation is display-only, on a grapheme boundary, with an ASCII
`.` mark; retain the complete source/copy/execute value. For English text, wrap
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
